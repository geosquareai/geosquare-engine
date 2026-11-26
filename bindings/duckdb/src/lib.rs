//! DuckDB loadable extension `geosquare_v2` for the GeoSquare V2 grid (spec 2.0).
//!
//! Built on the DuckDB C extension API (struct v1.2.0) so one binary loads in
//! DuckDB 1.4 through 2.0. SQL function names follow `geosquare-v2` and the
//! `geosquare-engine` bindings.

mod avro;
mod content_hash;
mod dataset;
mod detect_fn;
mod equal_area;
mod geom;
mod grid;
mod iceberg;
mod icefast;
mod inspect;
mod lines;
mod maintenance;
mod manifest11;
mod metadata;
mod polyfill;
mod raster;
mod registry;
mod sql;
mod storage;
mod tables;
mod compact;
mod compact_store;
mod geoagg;
mod timecube;
mod timequery;
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
        let pool = register_dataset_functions(db)?;
        con.register_table_function_with_extra_info::<raster::ReadRaster, _>("read_raster", &pool)?;
        Ok(true)
    }
}

fn s3_named() -> Vec<(&'static str, sql::Kind)> {
    use sql::Kind::Text;
    vec![
        ("s3_region", Text),
        ("s3_endpoint", Text),
        ("s3_url_style", Text),
        ("s3_use_ssl", Text),
        ("s3_access_key_id", Text),
        ("s3_secret_access_key", Text),
        ("s3_session_token", Text),
    ]
}

unsafe fn register_dataset_functions(
    db: ffi::duckdb_database,
) -> Result<std::sync::Arc<sql::Pool>, Box<dyn Error>> {
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
            ("snapshot_id", Int),
            ("as_of", Text),
            ("with_geometry", Text),
            ("geometry_crs", Text),
            ("rollup_level", Int),
            ("rollup_rule", Text),
            ("space_weight", Text),
        ]);
        let time_params = [
            ("period", Text),
            ("period_from", Text),
            ("period_to", Text),
            ("time_rollup", Text),
            ("time_rule", Text),
        ];
        read_params.extend(time_params);
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
                ("snapshot_id", Int),
                ("as_of", Text),
                ("geometry", Text),
                ("radius_m", Float),
                ("bbox", FloatList),
                ("min_coverage", Float),
                ("source_crs", Text),
                ("coverage_mode", Text),
                ("max_candidate_limit", Int),
                ("allow_candidate_crs", Bool),
                ("with_geometry", Text),
                ("geometry_crs", Text),
                ("rollup_level", Int),
                ("rollup_rule", Text),
                ("space_weight", Text),
            ]);
            q.extend(time_params);
            let mut qm = q;
            qm.push(("fill_nulls", Bool));
            // query_cell_dataset(path | [path, ...]): a list behaves as query_cell_datasets.
            sql::register(
                con,
                "query_cell_dataset",
                vec![TextOrList],
                qm.clone(),
                dataset::plan_query,
                pool.clone(),
            )?;
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
                ("space_aggregation", Text),
                ("space_weight", Text),
                ("manifest_version", Text),
                ("owner", Text),
                ("source_name", Text),
                ("license", Text),
                ("share_policy", Text),
                ("refresh", Text),
                ("derived_from", Text),
                ("period_type", Text),
                ("bucket_hours", Int),
                ("period_start", Text),
                ("period_end", Text),
                ("time_chunk", Text),
                ("time_aggregation", Text),
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
                "write_compact_set",
                vec![Text, Text],
                vec![("domain_code", Text), ("canonicalize", Bool), ("mode", Text)],
                compact_store::plan_write,
                pool.clone(),
            )?;
            sql::register(
                con,
                "aggregate_geometries_to_cells",
                vec![Text],
                vec![
                    ("domain_code", Text),
                    ("level", Int),
                    ("value_semantics", Text),
                    ("geometry_column", Text),
                    ("source_crs", Text),
                    ("id_column", Text),
                    ("category_rule", Text),
                    ("rate_parts", Text),
                    ("range_columns", Text),
                    ("ordinal_order", Text),
                ],
                geoagg::plan,
                pool.clone(),
            )?;
            sql::register(
                con,
                "read_compact_set",
                vec![Text],
                vec![("level", Int)],
                compact_store::plan_read,
                pool.clone(),
            )?;
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
            sql::register(
                con,
                "cell_dataset_content_hashes",
                vec![Text],
                identity.clone(),
                dataset::plan_content_hashes,
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
            let mut ex = vec![
                ("retain_last", Int),
                ("older_than", Text),
                ("orphan_age_hours", Float),
                ("dry_run", Bool),
            ];
            ex.extend(s3_named());
            for (name, extra, plan) in [
                (
                    "list_cell_datasets",
                    vec![],
                    inspect::plan_list as sql::Planner,
                ),
                (
                    "validate_cell_dataset",
                    vec![],
                    inspect::plan_validate as sql::Planner,
                ),
                (
                    "cell_dataset_stats",
                    vec![("exact", Bool)],
                    inspect::plan_stats as sql::Planner,
                ),
            ] {
                let mut named = extra;
                named.extend(s3_named());
                sql::register(con, name, vec![Text], named, plan, pool.clone())?;
            }
            let mut al = vec![
                ("drop_columns", Text),
                ("rename_columns", Text),
                ("add_columns", Text),
            ];
            al.extend(s3_named());
            sql::register(
                con,
                "alter_cell_dataset",
                vec![Text],
                al,
                writer::plan_alter,
                pool.clone(),
            )?;
            let mut cp = vec![("partition_level", Int)];
            cp.extend(s3_named());
            sql::register(
                con,
                "compact_cell_dataset",
                vec![Text],
                cp,
                writer::plan_compact,
                pool.clone(),
            )?;
            sql::register(
                con,
                "expire_snapshots",
                vec![Text],
                ex,
                maintenance::plan_expire,
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
        r.map(|_| pool)
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
