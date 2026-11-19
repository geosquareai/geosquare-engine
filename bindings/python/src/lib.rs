//! Python binding for the GeoSquare V2 grid core (spec 2.0).
//!
//! Every function is a thin wrapper. The rules live in `geosquare-core`, and the
//! conformance vectors are the contract. Invalid input raises an exception; no value is
//! repaired or guessed.

use geosquare_core::codec::{self, Cell};
use geosquare_core::detect::{self, Column, DType, Kind, Status, Value};
use geosquare_core::{domain, geo, packing, topology};
use pyo3::exceptions::{PyOverflowError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyInt, PyList, PyString};

fn value_error<E: std::fmt::Display>(error: E) -> PyErr {
    PyValueError::new_err(error.to_string())
}

fn cell_of(gid: &str) -> PyResult<Cell> {
    codec::decode(gid).map_err(value_error)
}

fn gid_of(cell: Cell) -> PyResult<String> {
    codec::encode(cell).map_err(value_error)
}

/// Canonical indices to GID. `level` must be 0 to 14.
#[pyfunction]
fn encode(level: u8, x_idx: u64, y_idx: u64) -> PyResult<String> {
    gid_of(Cell {
        level,
        x_idx,
        y_idx,
    })
}

/// GID to `(level, x_idx, y_idx)`. The level is the GID length.
#[pyfunction]
fn decode(gid: &str) -> PyResult<(u8, u64, u64)> {
    let cell = cell_of(gid)?;
    Ok((cell.level, cell.x_idx, cell.y_idx))
}

/// Pack a domain ID (1 to 511) and a GID into one signed Int64.
#[pyfunction]
fn pack(domain_id: u16, gid: &str) -> PyResult<i64> {
    let cell = cell_of(gid)?;
    packing::pack(domain_id, cell).map_err(value_error)
}

/// Unpack a signed Int64 into `(domain_id, gid)`. Any inconsistent value raises.
#[pyfunction]
fn unpack(packed: i64) -> PyResult<(u16, String)> {
    let (domain_id, cell) = packing::unpack(packed).map_err(value_error)?;
    Ok((domain_id, gid_of(cell)?))
}

/// Parent GID, or `None` for the level 0 cell.
#[pyfunction]
fn parent(gid: &str) -> PyResult<Option<String>> {
    match topology::parent(cell_of(gid)?) {
        Some(parent) => Ok(Some(gid_of(parent)?)),
        None => Ok(None),
    }
}

/// Child GIDs one level down: 25 for a 5x5 level, 4 for a 2x2 level.
#[pyfunction]
fn children(gid: &str) -> PyResult<Vec<String>> {
    topology::children(cell_of(gid)?)
        .map_err(value_error)?
        .into_iter()
        .map(gid_of)
        .collect()
}

/// Inclusive packed range `(lo, hi)` that holds every descendant of `parent_gid` at
/// `child_level`. Some values inside the range are not valid codes.
#[pyfunction]
fn descendant_range(domain_id: u16, parent_gid: &str, child_level: u8) -> PyResult<(i64, i64)> {
    let parent = cell_of(parent_gid)?;
    topology::descendant_range(domain_id, parent, child_level).map_err(value_error)
}

/// GIDs with Chebyshev distance exactly `k` from `gid`, clipped to the grid.
#[pyfunction]
fn k_ring(gid: &str, k: u64) -> PyResult<Vec<String>> {
    topology::k_ring(cell_of(gid)?, k)
        .map_err(value_error)?
        .into_iter()
        .map(gid_of)
        .collect()
}

/// GIDs with Chebyshev distance at most `k` from `gid`, including `gid`, clipped to the grid.
#[pyfunction]
fn k_disk(gid: &str, k: u64) -> PyResult<Vec<String>> {
    topology::k_disk(cell_of(gid)?, k)
        .map_err(value_error)?
        .into_iter()
        .map(gid_of)
        .collect()
}

/// Grid distance in metres between two GIDs on the same level.
#[pyfunction]
fn distance_m(a: &str, b: &str) -> PyResult<f64> {
    topology::distance_m(cell_of(a)?, cell_of(b)?).map_err(value_error)
}

/// Side length of a cell in metres at `level`.
#[pyfunction]
fn cell_side_m(level: u8) -> PyResult<f64> {
    codec::cell_side_m(level).map_err(value_error)
}

fn domain_of(domain_id: u16) -> PyResult<&'static domain::Domain> {
    domain::by_id(domain_id)
        .ok_or_else(|| PyValueError::new_err(format!("unknown domain ID {domain_id}")))
}

/// Return a closed WGS84 `(longitude, latitude)` ring for a GID.
///
/// `domain_id` selects the grid CRS because a bare GID does not contain a domain.
/// `max_segment_length_m` is the maximum projected edge segment length in metres.
/// Candidate CRS domains require explicit `allow_candidate_crs=True`.
#[pyfunction]
#[pyo3(signature = (domain_id, gid, max_segment_length_m=1000.0, allow_candidate_crs=false))]
fn cell_polygon_wgs84(
    domain_id: u16,
    gid: &str,
    max_segment_length_m: f64,
    allow_candidate_crs: bool,
) -> PyResult<Vec<(f64, f64)>> {
    geo::cell_polygon_wgs84_with_options(
        domain_of(domain_id)?,
        cell_of(gid)?,
        max_segment_length_m,
        allow_candidate_crs,
    )
    .map_err(value_error)
}

fn dtype_of(name: &str) -> PyResult<DType> {
    match name {
        "string" => Ok(DType::Str),
        "int64" => Ok(DType::Int64),
        "other" => Ok(DType::Other),
        _ => Err(PyValueError::new_err(format!(
            "dtype {name:?} is not one of \"string\", \"int64\", \"other\""
        ))),
    }
}

/// Convert one Python cell value. Only `str`, `int` and `None` are allowed. Floats,
/// bools and other types raise, so NaN must be turned into `None` by the caller.
fn value_of(column: &str, item: &Bound<'_, PyAny>) -> PyResult<Value> {
    if item.is_none() {
        return Ok(Value::Null);
    }
    if item.is_instance_of::<PyBool>() {
        return Err(PyTypeError::new_err(format!(
            "column {column:?}: bool values are not allowed"
        )));
    }
    if item.is_instance_of::<PyString>() {
        return Ok(Value::Str(item.extract::<String>()?));
    }
    if item.is_instance_of::<PyInt>() {
        return match item.extract::<i64>() {
            Ok(number) => Ok(Value::Int(number)),
            Err(_) => Err(PyOverflowError::new_err(format!(
                "column {column:?}: int value is outside the Int64 range"
            ))),
        };
    }
    Err(PyTypeError::new_err(format!(
        "column {column:?}: values must be str, int or None, not {}",
        item.get_type().name()?
    )))
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Uri => "uri",
        Kind::Gid => "gid",
        Kind::PackedId => "packed_id",
        Kind::XyIndices => "xy_indices",
        Kind::NotSpatial => "none",
    }
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Certain => "certain",
        Status::NeedsDomain => "needs_domain",
        Status::NeedsLevel => "needs_level",
        Status::NeedsConfirmation => "needs_confirmation",
        Status::Rejected => "rejected",
    }
}

/// Classify the columns of a table as spatial keys (spec 2.0, section 14).
///
/// `columns` maps each column name to `(dtype, values)`, where dtype is `"string"`,
/// `"int64"` or `"other"`. `domain` is the domain code from the file or the user, and
/// `level` is needed to judge `x_idx` and `y_idx`. Returns one dict per result.
#[pyfunction]
#[pyo3(signature = (columns, domain=None, level=None))]
fn detect_table<'py>(
    columns: &Bound<'py, PyDict>,
    domain: Option<String>,
    level: Option<u8>,
) -> PyResult<Vec<Bound<'py, PyDict>>> {
    let py = columns.py();
    let mut table = Vec::with_capacity(columns.len());
    for (name, spec) in columns.iter() {
        let name: String = name.extract()?;
        let parts: Vec<Bound<'py, PyAny>> = spec.extract()?;
        if parts.len() != 2 {
            return Err(PyValueError::new_err(format!(
                "column {name:?}: expected (dtype, values)"
            )));
        }
        let dtype = dtype_of(&parts[0].extract::<String>()?)?;
        let values = if dtype == DType::Other {
            // The detector rejects an "other" column by dtype alone and never reads its
            // values, so they are not converted. Only null-ness is kept.
            parts[1]
                .try_iter()?
                .map(|item| {
                    let item = item?;
                    Ok(if item.is_none() {
                        Value::Null
                    } else {
                        Value::Str(String::new())
                    })
                })
                .collect::<PyResult<Vec<Value>>>()?
        } else {
            parts[1]
                .try_iter()?
                .map(|item| value_of(&name, &item?))
                .collect::<PyResult<Vec<Value>>>()?
        };
        table.push(Column {
            name,
            dtype,
            values,
        });
    }

    let mut out = Vec::new();
    for result in detect::detect_table(&table, domain.as_deref(), level) {
        let dict = PyDict::new(py);
        dict.set_item("column", result.column)?;
        dict.set_item("kind", kind_name(result.kind))?;
        dict.set_item("status", status_name(result.status))?;
        dict.set_item("reason", result.reason)?;
        dict.set_item("valid_ratio", result.valid_ratio)?;
        dict.set_item("domain_codes", result.domain_codes)?;
        dict.set_item("domain_ids", result.domain_ids)?;
        // Vec<u8> would become Python bytes, so levels are built as a list of ints.
        dict.set_item("levels", PyList::new(py, result.levels)?)?;
        dict.set_item("invalid_examples", result.invalid_examples)?;
        out.push(dict);
    }
    Ok(out)
}

#[pymodule]
fn _core(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add_function(wrap_pyfunction!(encode, module)?)?;
    module.add_function(wrap_pyfunction!(decode, module)?)?;
    module.add_function(wrap_pyfunction!(pack, module)?)?;
    module.add_function(wrap_pyfunction!(unpack, module)?)?;
    module.add_function(wrap_pyfunction!(parent, module)?)?;
    module.add_function(wrap_pyfunction!(children, module)?)?;
    module.add_function(wrap_pyfunction!(descendant_range, module)?)?;
    module.add_function(wrap_pyfunction!(k_ring, module)?)?;
    module.add_function(wrap_pyfunction!(k_disk, module)?)?;
    module.add_function(wrap_pyfunction!(distance_m, module)?)?;
    module.add_function(wrap_pyfunction!(cell_side_m, module)?)?;
    module.add_function(wrap_pyfunction!(cell_polygon_wgs84, module)?)?;
    module.add_function(wrap_pyfunction!(detect_table, module)?)?;
    Ok(())
}
