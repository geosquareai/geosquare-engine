//! WebAssembly binding for the GeoSquare V2 grid core (spec 2.0).
//!
//! A packed ID (`Int64`) crosses into JavaScript as a `BigInt`. A `Number` is refused
//! wherever a packed ID is expected. Numeric arguments must be JavaScript numbers that
//! are whole and in range. Invalid input throws an `Error`. No value is wrapped, truncated
//! or repaired.

use geosquare_core::codec::{self, Cell};
use geosquare_core::detect::{self, Column, ColumnDetection, DType, Kind, Status, Value};
use geosquare_core::{packing, topology};
use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// Largest integer that a JavaScript `Number` holds exactly.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn js_error<E: std::fmt::Display>(error: E) -> JsError {
    JsError::new(&error.to_string())
}

fn cell_of(gid: &str) -> Result<Cell, JsError> {
    codec::decode(gid).map_err(js_error)
}

fn gid_of(cell: Cell) -> Result<String, JsError> {
    codec::encode(cell).map_err(js_error)
}

/// Reads a JavaScript `number`. `wasm-bindgen` would turn `null` into 0 and a numeric
/// string into a number, so the type is checked here.
fn number_arg(value: &JsValue, name: &str) -> Result<f64, JsError> {
    value
        .as_f64()
        .ok_or_else(|| JsError::new(&format!("{name} must be a number")))
}

/// Whole number that fits in `T`. A plain cast would wrap 256 to 0 for a `u8`, so this
/// throws instead.
fn whole<T: TryFrom<u64>>(value: &JsValue, name: &str) -> Result<T, JsError> {
    let number = number_arg(value, name)?;
    // `fract()` is NaN for NaN and infinities, so those fail the equality too.
    let is_whole = number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&number);
    if !is_whole {
        return Err(JsError::new(&format!(
            "{name} must be a whole number, got {number}"
        )));
    }
    T::try_from(number as u64)
        .map_err(|_| JsError::new(&format!("{name} is out of range: {number}")))
}

/// A `BigInt` argument that must fit in Int64. `wasm-bindgen` would wrap 2^64 + 5 to 5
/// silently, so the range is checked here.
fn packed_arg(value: &JsValue) -> Result<i64, JsError> {
    if !value.is_bigint() {
        return Err(JsError::new("a packed ID must be a BigInt, not a Number"));
    }
    i64::try_from(value.clone())
        .map_err(|_| JsError::new("packed ID BigInt is outside the Int64 range"))
}

/// A decoded cell. Indices are `u32`, because the largest grid edge is 10^7 cells.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug)]
pub struct GridCell {
    #[wasm_bindgen(readonly)]
    pub level: u8,
    #[wasm_bindgen(readonly, js_name = xIdx)]
    pub x_idx: u32,
    #[wasm_bindgen(readonly, js_name = yIdx)]
    pub y_idx: u32,
}

/// A packed ID unpacked into its domain ID and GID.
#[wasm_bindgen(getter_with_clone)]
pub struct GridId {
    #[wasm_bindgen(readonly, js_name = domainId)]
    pub domain_id: u16,
    pub gid: String,
}

/// Inclusive packed range `[lo, hi]`. Both values are `BigInt`.
#[wasm_bindgen]
pub struct DescendantRange {
    #[wasm_bindgen(readonly)]
    pub lo: i64,
    #[wasm_bindgen(readonly)]
    pub hi: i64,
}

/// Canonical indices to GID. `level` must be 0 to 14.
#[wasm_bindgen]
pub fn encode(
    #[wasm_bindgen(unchecked_param_type = "number")] level: JsValue,
    #[wasm_bindgen(unchecked_param_type = "number")] x_idx: JsValue,
    #[wasm_bindgen(unchecked_param_type = "number")] y_idx: JsValue,
) -> Result<String, JsError> {
    gid_of(Cell {
        level: whole(&level, "level")?,
        x_idx: whole(&x_idx, "xIdx")?,
        y_idx: whole(&y_idx, "yIdx")?,
    })
}

/// GID to `{ level, xIdx, yIdx }`. The level is the GID length.
#[wasm_bindgen]
pub fn decode(gid: &str) -> Result<GridCell, JsError> {
    let cell = cell_of(gid)?;
    Ok(GridCell {
        level: cell.level,
        x_idx: u32::try_from(cell.x_idx).map_err(js_error)?,
        y_idx: u32::try_from(cell.y_idx).map_err(js_error)?,
    })
}

/// Pack a domain ID (1 to 511) and a GID into one `BigInt`.
#[wasm_bindgen]
pub fn pack(
    #[wasm_bindgen(unchecked_param_type = "number")] domain_id: JsValue,
    gid: &str,
) -> Result<i64, JsError> {
    packing::pack(whole(&domain_id, "domainId")?, cell_of(gid)?).map_err(js_error)
}

/// Unpack a `BigInt` into `{ domainId, gid }`. Any inconsistent value throws.
#[wasm_bindgen]
pub fn unpack(
    #[wasm_bindgen(unchecked_param_type = "bigint")] packed: JsValue,
) -> Result<GridId, JsError> {
    let (domain_id, cell) = packing::unpack(packed_arg(&packed)?).map_err(js_error)?;
    Ok(GridId {
        domain_id,
        gid: gid_of(cell)?,
    })
}

/// Parent GID, or `undefined` for the level 0 cell.
#[wasm_bindgen]
pub fn parent(gid: &str) -> Result<Option<String>, JsError> {
    topology::parent(cell_of(gid)?).map(gid_of).transpose()
}

/// Child GIDs one level down: 25 for a 5x5 level, 4 for a 2x2 level.
#[wasm_bindgen]
pub fn children(gid: &str) -> Result<Vec<String>, JsError> {
    topology::children(cell_of(gid)?)
        .map_err(js_error)?
        .into_iter()
        .map(gid_of)
        .collect()
}

/// Packed range that holds every descendant of `parentGid` at `childLevel`.
#[wasm_bindgen(js_name = descendantRange)]
pub fn descendant_range(
    #[wasm_bindgen(unchecked_param_type = "number")] domain_id: JsValue,
    parent_gid: &str,
    #[wasm_bindgen(unchecked_param_type = "number")] child_level: JsValue,
) -> Result<DescendantRange, JsError> {
    let (lo, hi) = topology::descendant_range(
        whole(&domain_id, "domainId")?,
        cell_of(parent_gid)?,
        whole(&child_level, "childLevel")?,
    )
    .map_err(js_error)?;
    Ok(DescendantRange { lo, hi })
}

/// GIDs with Chebyshev distance exactly `k` from `gid`, clipped to the grid.
#[wasm_bindgen(js_name = kRing)]
pub fn k_ring(
    gid: &str,
    #[wasm_bindgen(unchecked_param_type = "number")] k: JsValue,
) -> Result<Vec<String>, JsError> {
    topology::k_ring(cell_of(gid)?, whole(&k, "k")?)
        .map_err(js_error)?
        .into_iter()
        .map(gid_of)
        .collect()
}

/// GIDs with Chebyshev distance at most `k` from `gid`, including `gid`, clipped to the grid.
#[wasm_bindgen(js_name = kDisk)]
pub fn k_disk(
    gid: &str,
    #[wasm_bindgen(unchecked_param_type = "number")] k: JsValue,
) -> Result<Vec<String>, JsError> {
    topology::k_disk(cell_of(gid)?, whole(&k, "k")?)
        .map_err(js_error)?
        .into_iter()
        .map(gid_of)
        .collect()
}

/// Grid distance in metres between two GIDs on the same level.
#[wasm_bindgen(js_name = distanceM)]
pub fn distance_m(a: &str, b: &str) -> Result<f64, JsError> {
    topology::distance_m(cell_of(a)?, cell_of(b)?).map_err(js_error)
}

/// Side length of a cell in metres at `level`.
#[wasm_bindgen(js_name = cellSideM)]
pub fn cell_side_m(
    #[wasm_bindgen(unchecked_param_type = "number")] level: JsValue,
) -> Result<f64, JsError> {
    codec::cell_side_m(whole(&level, "level")?).map_err(js_error)
}

/// Version of this binding.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn dtype_of(name: &str) -> Result<DType, JsError> {
    match name {
        "string" => Ok(DType::Str),
        "int64" => Ok(DType::Int64),
        "other" => Ok(DType::Other),
        _ => Err(JsError::new(&format!(
            "dtype {name:?} is not one of \"string\", \"int64\", \"other\""
        ))),
    }
}

/// Convert one JavaScript cell value.
///
/// Accepted: `null` or `undefined`, a string, a `BigInt` (inside Int64), or a whole
/// `Number` up to 2^53 - 1. Anything else throws, so NaN and fractions cannot slip in.
fn value_of(column: &str, dtype: DType, item: &JsValue) -> Result<Value, JsError> {
    if item.is_null() || item.is_undefined() {
        return Ok(Value::Null);
    }
    if dtype == DType::Other {
        // The detector rejects an "other" column by dtype alone and never reads its values.
        return Ok(Value::Str(String::new()));
    }
    if let Some(text) = item.as_string() {
        return Ok(Value::Str(text));
    }
    if item.is_bigint() {
        return i64::try_from(item.clone()).map(Value::Int).map_err(|_| {
            JsError::new(&format!(
                "column {column:?}: BigInt is outside the Int64 range"
            ))
        });
    }
    if let Some(number) = item.as_f64() {
        if !number.is_finite() || number.fract() != 0.0 {
            return Err(JsError::new(&format!(
                "column {column:?}: numbers must be whole; use null for missing values"
            )));
        }
        if number.abs() > MAX_SAFE_INTEGER {
            return Err(JsError::new(&format!(
                "column {column:?}: a Number above 2^53 loses precision; pass a BigInt"
            )));
        }
        return Ok(Value::Int(number as i64));
    }
    Err(JsError::new(&format!(
        "column {column:?}: values must be a string, BigInt, whole number or null"
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

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsError> {
    Reflect::set(object, &JsValue::from_str(key), value)
        .map(|_| ())
        .map_err(|_| JsError::new(&format!("cannot set property {key:?}")))
}

fn string_array(items: &[String]) -> Array {
    items.iter().map(|text| JsValue::from_str(text)).collect()
}

fn number_array(items: impl Iterator<Item = f64>) -> Array {
    items.map(JsValue::from_f64).collect()
}

fn detection_object(result: &ColumnDetection) -> Result<Object, JsError> {
    let object = Object::new();
    set(&object, "column", &JsValue::from_str(&result.column))?;
    set(&object, "kind", &JsValue::from_str(kind_name(result.kind)))?;
    set(
        &object,
        "status",
        &JsValue::from_str(status_name(result.status)),
    )?;
    set(&object, "reason", &JsValue::from_str(&result.reason))?;
    set(
        &object,
        "validRatio",
        &JsValue::from_f64(result.valid_ratio),
    )?;
    set(&object, "domainCodes", &string_array(&result.domain_codes))?;
    set(
        &object,
        "domainIds",
        &number_array(result.domain_ids.iter().map(|&id| f64::from(id))),
    )?;
    set(
        &object,
        "levels",
        &number_array(result.levels.iter().map(|&level| f64::from(level))),
    )?;
    set(
        &object,
        "invalidExamples",
        &string_array(&result.invalid_examples),
    )?;
    Ok(object)
}

/// Classify the columns of a table as spatial keys (spec 2.0, section 14).
///
/// `columns` is an object that maps each column name to `[dtype, values]`. The dtype is
/// `"string"`, `"int64"` or `"other"`. Int64 values are `BigInt` (or whole `Number`s up to
/// 2^53). `domain` is the domain code from the file or the user. `level` is needed to judge
/// `xIdx` and `yIdx`. Returns one object per result.
#[wasm_bindgen(js_name = detectTable)]
pub fn detect_table(
    columns: Object,
    domain: Option<String>,
    #[wasm_bindgen(unchecked_param_type = "number | null | undefined")] level: JsValue,
) -> Result<Array, JsError> {
    let level: Option<u8> = if level.is_null() || level.is_undefined() {
        None
    } else {
        Some(whole(&level, "level")?)
    };

    let mut table = Vec::new();
    for entry in Object::entries(&columns).iter() {
        let pair: Array = entry.unchecked_into();
        let name = pair
            .get(0)
            .as_string()
            .ok_or_else(|| JsError::new("column name must be a string"))?;
        let spec = pair.get(1);
        if !Array::is_array(&spec) {
            return Err(JsError::new(&format!(
                "column {name:?}: expected [dtype, values]"
            )));
        }
        let spec: Array = spec.unchecked_into();
        if spec.length() != 2 {
            return Err(JsError::new(&format!(
                "column {name:?}: expected [dtype, values]"
            )));
        }
        let dtype_name = spec
            .get(0)
            .as_string()
            .ok_or_else(|| JsError::new(&format!("column {name:?}: dtype must be a string")))?;
        let dtype = dtype_of(&dtype_name)?;
        let values_js = spec.get(1);
        if !Array::is_array(&values_js) {
            return Err(JsError::new(&format!(
                "column {name:?}: values must be an array"
            )));
        }
        let values_array: Array = values_js.unchecked_into();
        let values = values_array
            .iter()
            .map(|item| value_of(&name, dtype, &item))
            .collect::<Result<Vec<Value>, JsError>>()?;
        table.push(Column {
            name,
            dtype,
            values,
        });
    }

    let out = Array::new();
    for result in detect::detect_table(&table, domain.as_deref(), level) {
        let object = detection_object(&result)?;
        out.push(&object);
    }
    Ok(out)
}
