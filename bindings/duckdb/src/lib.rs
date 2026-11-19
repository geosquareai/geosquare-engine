//! DuckDB loadable extension `geosquare_v2` for the GeoSquare V2 grid (spec 2.0).
//!
//! Built on the DuckDB C extension API (struct v1.2.0) so one binary loads in
//! DuckDB 1.4 through 2.0. SQL function names follow `geosquare-v2` and the
//! `geosquare-engine` bindings.

mod avro;
mod dataset;
mod detect_fn;
mod equal_area;
mod geom;
mod grid;
mod iceberg;
mod icefast;
mod lines;
mod metadata;
mod polyfill;
mod sql;
mod storage;
mod tables;
mod v1;
mod valid;
mod writer;

use duckdb::{ffi, Connection};
use std::{error::Error, ffi::CString};

/// Minimum C API struct this extension asks DuckDB for.
const MIN_CAPI: &str = "v1.2.0";
/// Internal connections for dataset functions: one per concurrent dataset scan/write.
/// They must be opened during LOAD (the loader's database handle is not valid later),
/// so the size is fixed per process. Override with GEOSQUARE_V2_CONNECTIONS (1..=1024).
const DEFAULT_POOL_SIZE: usize = 32;

fn pool_size() -> usize {
    std::env::var("GEOSQUARE_V2_CONNECTIONS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| (1..=1024).contains(n))
        .unwrap_or(DEFAULT_POOL_SIZE)
}

unsafe fn init(
    info: ffi::duckdb_extension_info,
    access: *const ffi::duckdb_extension_access,
) -> Result<bool, Box<dyn Error>> {
    unsafe {
        if !ffi::duckdb_rs_extension_api_init(info, access, MIN_CAPI)? {
            return Ok(false);
        }
        let db_ptr = ((*access).get_database.ok_or("get_database is null")?)(info);
        if db_ptr.is_null() {
            return Ok(false);
        }
        let db: ffi::duckdb_database = *db_ptr;
        let con = Connection::open_from_raw(db.cast())?;
        grid::register(&con)?;
        con.register_table_function::<tables::PolygonToCells>("polygon_to_cells")?;
        con.register_table_function::<tables::Polyfill>("polyfill")?;
        con.register_table_function::<tables::LineToCells>("line_to_cells")?;
        register_dataset_functions(db)?;
        Ok(true)
    }
}

unsafe fn register_dataset_functions(db: ffi::duckdb_database) -> Result<(), Box<dyn Error>> {
    use sql::Kind::*;
    unsafe {
        let pool = sql::init_pool(db, pool_size())?;
        let mut con: ffi::duckdb_connection = std::ptr::null_mut();
        if ffi::duckdb_connect(db, &mut con) != ffi::duckdb_state_DuckDBSuccess {
            return Err("could not open a registration connection".into());
        }
        let identity = vec![("domain_code", Text), ("level", Int)];
        let mut read_params = identity.clone();
        read_params.extend([
            ("with_geometry", Text),
            ("geometry_crs", Text),
            ("rollup_level", Int),
            ("rollup_rule", Text),
        ]);
        let r = (|| -> Result<(), Box<dyn Error>> {
            sql::register(
                con,
                "read_cell_dataset",
                vec![Text],
                read_params,
                dataset::plan_read,
                pool.clone(),
            )?;
            let mut q = identity.clone();
            q.extend([
                ("geometry", Text),
                ("bbox", FloatList),
                ("min_coverage", Float),
                ("source_crs", Text),
                ("coverage_mode", Text),
                ("max_candidate_limit", Int),
                ("with_geometry", Text),
                ("geometry_crs", Text),
                ("rollup_level", Int),
                ("rollup_rule", Text),
            ]);
            sql::register(
                con,
                "query_cell_dataset",
                vec![Text],
                q.clone(),
                dataset::plan_query,
                pool.clone(),
            )?;
            let mut qm = q;
            qm.push(("fill_nulls", Bool));
            sql::register(
                con,
                "query_cell_datasets",
                vec![TextList],
                qm,
                dataset::plan_query_multi,
                pool.clone(),
            )?;
            let mut w = identity.clone();
            w.extend([
                ("layout", Text),
                ("mode", Text),
                ("partition_level", Int),
                ("row_group_size", Int),
                ("value_semantics", Text),
                ("aggregation_rule", Text),
                ("value_fields", Text),
                ("coverage", Text),
                ("source", Text),
                ("notes", Text),
                ("identity", Text),
                ("column_metrics", Text),
                ("units", Text),
                ("column_descriptions", Text),
                ("title", Text),
                ("description", Text),
                ("data_period", Text),
                ("data_version", Text),
                ("fill_nulls", Bool),
                ("rollup_level", Int),
                ("s3_region", Text),
                ("s3_endpoint", Text),
                ("s3_url_style", Text),
                ("s3_use_ssl", Text),
                ("s3_access_key_id", Text),
                ("s3_secret_access_key", Text),
                ("s3_session_token", Text),
            ]);
            sql::register(
                con,
                "cell_dataset_info",
                vec![Text],
                identity.clone(),
                dataset::plan_info,
                pool.clone(),
            )?;
            sql::register(
                con,
                "cell_dataset_columns",
                vec![Text],
                identity.clone(),
                dataset::plan_columns,
                pool.clone(),
            )?;
            let mut u = vec![
                ("units", Text),
                ("column_descriptions", Text),
                ("title", Text),
                ("description", Text),
                ("data_period", Text),
                ("data_version", Text),
                ("source", Text),
                ("coverage", Text),
                ("notes", Text),
                ("s3_region", Text),
                ("s3_endpoint", Text),
                ("s3_url_style", Text),
                ("s3_use_ssl", Text),
                ("s3_access_key_id", Text),
                ("s3_secret_access_key", Text),
                ("s3_session_token", Text),
            ];
            u.shrink_to_fit();
            sql::register(
                con,
                "update_cell_metadata",
                vec![Text],
                u,
                writer::plan_update,
                pool.clone(),
            )?;
            sql::register(
                con,
                "detect_table",
                vec![Text],
                vec![("domain_code", Text), ("level", Int), ("max_rows", Int)],
                detect_fn::plan,
                pool.clone(),
            )?;
            sql::register(
                con,
                "write_cell_dataset",
                vec![Text, Text],
                w,
                writer::plan_write,
                pool.clone(),
            )?;
            Ok(())
        })();
        ffi::duckdb_disconnect(&mut con);
        r
    }
}

/// Entry point called by DuckDB on `LOAD geosquare_v2`.
///
/// # Safety
/// Called by DuckDB with valid extension info and access pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn geosquare_v2_init_c_api(
    info: ffi::duckdb_extension_info,
    access: *const ffi::duckdb_extension_access,
) -> bool {
    unsafe {
        match init(info, access) {
            Ok(v) => v,
            Err(e) => {
                if let Some(set_error) = (*access).set_error {
                    let msg = CString::new(format!("geosquare_v2: {e}")).unwrap_or_default();
                    set_error(info, msg.as_ptr());
                }
                false
            }
        }
    }
}
