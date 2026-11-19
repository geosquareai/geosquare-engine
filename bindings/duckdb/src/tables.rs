//! Table functions `polygon_to_cells` and `polyfill` (names from geosquare-v2).

use crate::{
    geom,
    grid::{domain_by_code, level_u8, uri},
    polyfill::{self, PolyfillOptions},
};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab, Value},
};
use geosquare_core::{codec, domain::Domain, packing, Cell};
use std::{
    error::Error,
    sync::atomic::{AtomicUsize, Ordering},
};

type E = Box<dyn Error>;
const CHUNK: usize = 2048;

fn named(bind: &BindInfo, name: &str) -> Option<Value> {
    bind.get_named_parameter(name).filter(|v| !v.is_null())
}

/// Read the shared polyfill options from named parameters.
pub fn options(bind: &BindInfo) -> PolyfillOptions {
    let mut o = PolyfillOptions::default();
    if let Some(v) = named(bind, "source_crs") {
        o.source_crs = v.to_string();
    }
    if let Some(v) = named(bind, "coverage_mode") {
        o.coverage_mode = v.to_string();
    }
    if let Some(v) = named(bind, "min_coverage") {
        o.min_coverage = v.to_double();
    }
    if let Some(v) = named(bind, "max_candidate_limit") {
        o.max_candidate_limit = v.to_int64();
    }
    if let Some(v) = named(bind, "allow_candidate_crs") {
        o.allow_candidate_crs = v.to_string().eq_ignore_ascii_case("true");
    }
    o
}

pub fn option_parameters() -> Vec<(String, LogicalTypeHandle)> {
    vec![
        ("source_crs".into(), LogicalTypeId::Varchar.into()),
        ("coverage_mode".into(), LogicalTypeId::Varchar.into()),
        ("min_coverage".into(), LogicalTypeId::Double.into()),
        ("max_candidate_limit".into(), LogicalTypeId::Bigint.into()),
        ("allow_candidate_crs".into(), LogicalTypeId::Boolean.into()),
    ]
}

type Cells = (&'static Domain, Vec<(Cell, f64)>);

/// Run polyfill for `(domain_code, wkt, level)` positional parameters.
fn run(bind: &BindInfo) -> Result<Cells, E> {
    let p0 = bind.get_parameter(0);
    let p1 = bind.get_parameter(1);
    let p2 = bind.get_parameter(2);
    if p0.is_null() || p1.is_null() || p2.is_null() {
        return Err("domain_code, geometry and level must not be NULL".into());
    }
    let d = domain_by_code(&p0.to_string())?;
    let level = level_u8(p2.to_int32())?;
    let polys = geom::parse_wkt(&p1.to_string())?;
    let cells = polyfill::polyfill(d, polys, level, &options(bind))?;
    Ok((d, cells))
}

pub struct Rows {
    domain: &'static Domain,
    cells: Vec<(Cell, f64)>,
}

pub struct Cursor {
    next: AtomicUsize,
}

fn init_cursor(_: &InitInfo) -> Result<Cursor, E> {
    Ok(Cursor {
        next: AtomicUsize::new(0),
    })
}

/// The cell record columns plus one ratio column.
fn cell_columns(bind: &BindInfo, ratio: &str) {
    for (name, t) in [
        ("domain_code", LogicalTypeId::Varchar),
        ("level", LogicalTypeId::Integer),
        ("x_idx", LogicalTypeId::Bigint),
        ("y_idx", LogicalTypeId::Bigint),
        ("gid", LogicalTypeId::Varchar),
        ("uri", LogicalTypeId::Varchar),
        ("packed_id", LogicalTypeId::Bigint),
        (ratio, LogicalTypeId::Double),
    ] {
        bind.add_result_column(name, t.into());
    }
}

/// Write the next chunk of cells (shared by polygon_to_cells and line_to_cells).
fn emit_cells(rows: &Rows, cursor: &Cursor, out: &mut DataChunkHandle) -> Result<(), E> {
    {
        let start = cursor
            .next
            .fetch_add(CHUNK, Ordering::Relaxed)
            .min(rows.cells.len());
        let end = (start + CHUNK).min(rows.cells.len());
        let n = end - start;
        let (code, lvl, xs, ys, gid, u, pk, cov) = (
            out.flat_vector(0),
            out.flat_vector(1),
            out.flat_vector(2),
            out.flat_vector(3),
            out.flat_vector(4),
            out.flat_vector(5),
            out.flat_vector(6),
            out.flat_vector(7),
        );
        let (mut lvl, mut xs, mut ys, mut pk, mut cov) = (lvl, xs, ys, pk, cov);
        for (i, (c, r)) in rows.cells[start..end].iter().enumerate() {
            let g = codec::encode(*c)?;
            code.insert(i, rows.domain.code);
            unsafe {
                lvl.as_mut_slice::<i32>()[i] = c.level as i32;
                xs.as_mut_slice::<i64>()[i] = c.x_idx as i64;
                ys.as_mut_slice::<i64>()[i] = c.y_idx as i64;
                pk.as_mut_slice::<i64>()[i] = packing::pack(rows.domain.id, *c)?;
                cov.as_mut_slice::<f64>()[i] = *r;
            }
            u.insert(i, uri(rows.domain.code, &g).as_str());
            gid.insert(i, g.as_str());
        }
        out.set_len(n);
        Ok(())
    }
}

/// polygon_to_cells(domain_code, geometry_wkt, level, source_crs := 'EPSG:4326',
///   coverage_mode := 'GRID_PLANAR', min_coverage := 0, max_candidate_limit := 100000)
/// -> (domain_code, level, x_idx, y_idx, gid, uri, packed_id, coverage_ratio), row-major order.
pub struct PolygonToCells;
impl VTab for PolygonToCells {
    type BindData = Rows;
    type InitData = Cursor;
    fn bind(bind: &BindInfo) -> Result<Rows, E> {
        cell_columns(bind, "coverage_ratio");
        let (domain, cells) = run(bind)?;
        Ok(Rows { domain, cells })
    }
    fn init(info: &InitInfo) -> Result<Cursor, E> {
        init_cursor(info)
    }
    fn func(f: &TableFunctionInfo<Self>, out: &mut DataChunkHandle) -> Result<(), E> {
        emit_cells(f.get_bind_data(), f.get_init_data(), out)
    }
    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![
            LogicalTypeId::Varchar.into(),
            LogicalTypeId::Varchar.into(),
            LogicalTypeId::Integer.into(),
        ])
    }
    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(option_parameters())
    }
}

/// polyfill(domain_code, geometry_wkt, level, ...same options) -> (gid, coverage_ratio)
pub struct Polyfill;
impl VTab for Polyfill {
    type BindData = Rows;
    type InitData = Cursor;
    fn bind(bind: &BindInfo) -> Result<Rows, E> {
        bind.add_result_column("gid", LogicalTypeId::Varchar.into());
        bind.add_result_column("coverage_ratio", LogicalTypeId::Double.into());
        let (domain, cells) = run(bind)?;
        Ok(Rows { domain, cells })
    }
    fn init(info: &InitInfo) -> Result<Cursor, E> {
        init_cursor(info)
    }
    fn func(f: &TableFunctionInfo<Self>, out: &mut DataChunkHandle) -> Result<(), E> {
        let rows = f.get_bind_data();
        let start = f
            .get_init_data()
            .next
            .fetch_add(CHUNK, Ordering::Relaxed)
            .min(rows.cells.len());
        let end = (start + CHUNK).min(rows.cells.len());
        let gid = out.flat_vector(0);
        let mut cov = out.flat_vector(1);
        for (i, (c, r)) in rows.cells[start..end].iter().enumerate() {
            gid.insert(i, codec::encode(*c)?.as_str());
            unsafe { cov.as_mut_slice::<f64>()[i] = *r };
        }
        out.set_len(end - start);
        Ok(())
    }
    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![
            LogicalTypeId::Varchar.into(),
            LogicalTypeId::Varchar.into(),
            LogicalTypeId::Integer.into(),
        ])
    }
    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(option_parameters())
    }
}

/// line_to_cells(domain_code, geometry_wkt, level, source_crs := 'EPSG:4326',
///   max_candidate_limit := 100000)
/// -> (domain_code, level, x_idx, y_idx, gid, uri, packed_id, length_ratio), row-major order.
/// `length_ratio` is the share of the whole line's length inside the cell (as in v2).
pub struct LineToCells;
impl VTab for LineToCells {
    type BindData = Rows;
    type InitData = Cursor;
    fn bind(bind: &BindInfo) -> Result<Rows, E> {
        cell_columns(bind, "length_ratio");
        let (p0, p1, p2) = (
            bind.get_parameter(0),
            bind.get_parameter(1),
            bind.get_parameter(2),
        );
        if p0.is_null() || p1.is_null() || p2.is_null() {
            return Err("domain_code, geometry and level must not be NULL".into());
        }
        let domain = domain_by_code(&p0.to_string())?;
        let level = level_u8(p2.to_int32())?;
        let lines = match geom::parse_geometry(&p1.to_string())? {
            geom::Geometry::Lines(l) => l,
            g => {
                return Err(format!(
                    "line_to_cells needs a LINESTRING or MULTILINESTRING, got a {}",
                    g.kind()
                )
                .into())
            }
        };
        let o = options(bind);
        let cells = crate::lines::line_to_cells(
            domain,
            &lines,
            level,
            &o.source_crs,
            o.max_candidate_limit,
        )?;
        Ok(Rows { domain, cells })
    }
    fn init(info: &InitInfo) -> Result<Cursor, E> {
        init_cursor(info)
    }
    fn func(f: &TableFunctionInfo<Self>, out: &mut DataChunkHandle) -> Result<(), E> {
        emit_cells(f.get_bind_data(), f.get_init_data(), out)
    }
    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![
            LogicalTypeId::Varchar.into(),
            LogicalTypeId::Varchar.into(),
            LogicalTypeId::Integer.into(),
        ])
    }
    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![
            ("source_crs".into(), LogicalTypeId::Varchar.into()),
            ("max_candidate_limit".into(), LogicalTypeId::Bigint.into()),
        ])
    }
}
