//! Scalar SQL functions. Names and argument order follow `geosquare-v2` and the
//! `geosquare-engine` Python binding. Invalid input raises an error; NULL input gives NULL.

use crate::geom;
use duckdb::{
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeHandle, LogicalTypeId},
    types::DuckString,
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
    Connection,
};
use geosquare_core::{codec, domain, geo, packing, topology, Cell};
use std::error::Error;

type R = Result<(), Box<dyn Error>>;

pub const URI_PREFIX: &str = "geosquare:v2:";

// ---------- input helpers ----------

fn is_null(v: &FlatVector, i: usize) -> bool {
    v.row_is_null(i as u64)
}

fn strings(chunk: &DataChunkHandle, col: usize, n: usize) -> Vec<Option<String>> {
    let v = chunk.flat_vector(col);
    let raw = unsafe { v.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(n) };
    (0..n)
        .map(|i| {
            if is_null(&v, i) {
                None
            } else {
                let mut s = raw[i];
                Some(DuckString::new(&mut s).as_str().to_string())
            }
        })
        .collect()
}

fn prims<T: Copy>(chunk: &DataChunkHandle, col: usize, n: usize) -> Vec<Option<T>> {
    let v = chunk.flat_vector(col);
    let raw = unsafe { v.as_slice_with_len::<T>(n) };
    (0..n)
        .map(|i| if is_null(&v, i) { None } else { Some(raw[i]) })
        .collect()
}

fn col_type(chunk: &DataChunkHandle, col: usize) -> LogicalTypeId {
    chunk.flat_vector(col).logical_type().id()
}

/// Domain codes or IDs as given in column `col` (VARCHAR code or INTEGER id).
fn domains(
    chunk: &DataChunkHandle,
    col: usize,
    n: usize,
) -> Result<Vec<Option<&'static domain::Domain>>, Box<dyn Error>> {
    let mut out = Vec::with_capacity(n);
    if col_type(chunk, col) == LogicalTypeId::Varchar {
        for c in strings(chunk, col, n) {
            out.push(match c {
                None => None,
                Some(code) => Some(domain_by_code(&code)?),
            });
        }
    } else {
        for id in prims::<i32>(chunk, col, n) {
            out.push(match id {
                None => None,
                Some(id) => Some(domain_by_id(id)?),
            });
        }
    }
    Ok(out)
}

pub fn domain_by_code(code: &str) -> Result<&'static domain::Domain, Box<dyn Error>> {
    domain::by_code(code).ok_or_else(|| format!("unknown domain_code {code:?}").into())
}

pub fn domain_by_id(id: i32) -> Result<&'static domain::Domain, Box<dyn Error>> {
    u16::try_from(id)
        .ok()
        .and_then(domain::by_id)
        .ok_or_else(|| format!("unknown domain_id {id}").into())
}

pub fn level_u8(level: i32) -> Result<u8, Box<dyn Error>> {
    u8::try_from(level)
        .ok()
        .filter(|l| *l <= codec::MAX_LEVEL)
        .ok_or_else(|| {
            format!(
                "level must be an integer in [0, {}]; received {level}",
                codec::MAX_LEVEL
            )
            .into()
        })
}

fn index_u64(v: i64, name: &str) -> Result<u64, Box<dyn Error>> {
    u64::try_from(v)
        .map_err(|_| format!("{name} must be a non-negative integer; received {v}").into())
}

pub fn uri(code: &str, gid: &str) -> String {
    format!("{URI_PREFIX}{code}:{gid}")
}

/// Split `geosquare:v2:<domain>:<gid>` (the level-0 GID is empty).
pub fn parse_uri_str(value: &str) -> Result<(&'static domain::Domain, String), Box<dyn Error>> {
    let rest = value
        .strip_prefix(URI_PREFIX)
        .ok_or_else(|| format!("URI must start with {URI_PREFIX:?}: {value:?}"))?;
    let (code, gid) = rest
        .split_once(':')
        .ok_or_else(|| format!("URI must be geosquare:v2:<domain>:<gid>: {value:?}"))?;
    let d = domain_by_code(code)?;
    codec::decode(gid)?;
    Ok((d, gid.to_string()))
}

// ---------- output helpers ----------

fn type_id(t: LogicalTypeId) -> LogicalTypeHandle {
    LogicalTypeHandle::from(t)
}

fn put_i64(v: &mut FlatVector, i: usize, x: i64) {
    unsafe { v.as_mut_slice::<i64>()[i] = x };
}
fn put_i32(v: &mut FlatVector, i: usize, x: i32) {
    unsafe { v.as_mut_slice::<i32>()[i] = x };
}
fn put_f64(v: &mut FlatVector, i: usize, x: f64) {
    unsafe { v.as_mut_slice::<f64>()[i] = x };
}
fn put_bool(v: &mut FlatVector, i: usize, x: bool) {
    unsafe { v.as_mut_slice::<bool>()[i] = x };
}

/// A flat output column for row-wise writing (VARCHAR / BIGINT / INTEGER / DOUBLE).
fn finish_scalar<T>(
    out: &mut dyn WritableVector,
    rows: Vec<Option<T>>,
    write: impl Fn(&mut FlatVector, usize, T),
) {
    let mut v = out.flat_vector();
    for (i, r) in rows.into_iter().enumerate() {
        match r {
            Some(x) => write(&mut v, i, x),
            None => v.set_null(i),
        }
    }
}

/// Values of one STRUCT field for every row.
enum Field {
    Str(Vec<String>),
    I64(Vec<i64>),
    I32(Vec<i32>),
}

fn finish_struct(out: &mut dyn WritableVector, valid: &[bool], fields: Vec<Field>) {
    let n = valid.len();
    let mut sv = out.struct_vector();
    for (k, f) in fields.into_iter().enumerate() {
        let mut c = sv.child(k, n);
        match f {
            Field::Str(v) => v
                .iter()
                .enumerate()
                .for_each(|(i, s)| c.insert(i, s.as_str())),
            Field::I64(v) => v
                .iter()
                .enumerate()
                .for_each(|(i, x)| put_i64(&mut c, i, *x)),
            Field::I32(v) => v
                .iter()
                .enumerate()
                .for_each(|(i, x)| put_i32(&mut c, i, *x)),
        }
        for (i, ok) in valid.iter().enumerate() {
            if !ok {
                c.set_null(i);
            }
        }
    }
    for (i, ok) in valid.iter().enumerate() {
        if !ok {
            sv.set_null(i);
        }
    }
}

fn finish_list(out: &mut dyn WritableVector, rows: Vec<Option<Vec<String>>>) {
    let mut lv = out.list_vector();
    let total: usize = rows.iter().map(|r| r.as_ref().map_or(0, |v| v.len())).sum();
    let child = lv.child(total);
    let mut offset = 0;
    for (i, r) in rows.iter().enumerate() {
        match r {
            Some(items) => {
                for (k, s) in items.iter().enumerate() {
                    child.insert(offset + k, s.as_str());
                }
                lv.set_entry(i, offset, items.len());
                offset += items.len();
            }
            None => {
                lv.set_entry(i, offset, 0);
                lv.set_null(i);
            }
        }
    }
    lv.set_len(total);
}

fn struct_type(fields: &[(&str, LogicalTypeId)]) -> LogicalTypeHandle {
    let f: Vec<(&str, LogicalTypeHandle)> = fields.iter().map(|(n, t)| (*n, type_id(*t))).collect();
    LogicalTypeHandle::struct_type(&f)
}

fn sig(params: &[LogicalTypeId], ret: LogicalTypeHandle) -> ScalarFunctionSignature {
    ScalarFunctionSignature::exact(params.iter().map(|t| type_id(*t)).collect(), ret)
}

use LogicalTypeId::{Bigint, Boolean, Double, Integer, Varchar};

const CELL_FIELDS: &[(&str, LogicalTypeId)] = &[
    ("domain_code", Varchar),
    ("level", Integer),
    ("x_idx", Bigint),
    ("y_idx", Bigint),
    ("gid", Varchar),
    ("uri", Varchar),
    ("packed_id", Bigint),
];

/// Write rows of `(domain, cell)` as the CELL_FIELDS struct.
fn finish_cells(
    out: &mut dyn WritableVector,
    rows: Vec<Option<(&'static domain::Domain, Cell)>>,
) -> R {
    let n = rows.len();
    let mut valid = vec![false; n];
    let (mut code, mut lvl, mut xs, mut ys, mut gids, mut uris, mut packs) = (
        vec![String::new(); n],
        vec![0; n],
        vec![0; n],
        vec![0; n],
        vec![String::new(); n],
        vec![String::new(); n],
        vec![0; n],
    );
    for (i, r) in rows.into_iter().enumerate() {
        if let Some((d, c)) = r {
            let gid = codec::encode(c)?;
            valid[i] = true;
            code[i] = d.code.to_string();
            lvl[i] = c.level as i32;
            xs[i] = c.x_idx as i64;
            ys[i] = c.y_idx as i64;
            uris[i] = uri(d.code, &gid);
            packs[i] = packing::pack(d.id, c)?;
            gids[i] = gid;
        }
    }
    finish_struct(
        out,
        &valid,
        vec![
            Field::Str(code),
            Field::I32(lvl),
            Field::I64(xs),
            Field::I64(ys),
            Field::Str(gids),
            Field::Str(uris),
            Field::I64(packs),
        ],
    );
    Ok(())
}

// ---------- functions ----------

/// point_to_cell(domain_code, longitude, latitude, level [, allow_candidate_crs])
/// -> STRUCT(domain_code, level, x_idx, y_idx, gid, uri, packed_id)
///
/// Candidate CRS use is refused unless the fifth argument is true.
pub struct PointToCell;
impl VScalar for PointToCell {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (d, lon, lat, lvl) = (
            domains(input, 0, n)?,
            prims::<f64>(input, 1, n),
            prims::<f64>(input, 2, n),
            prims::<i32>(input, 3, n),
        );
        let allow_candidate = if input.num_columns() > 4 {
            prims::<bool>(input, 4, n)
        } else {
            vec![Some(false); n]
        };
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(match (d[i], lon[i], lat[i], lvl[i], allow_candidate[i]) {
                (Some(d), Some(lo), Some(la), Some(l), Some(allow_candidate)) => Some((
                    d,
                    geo::point_to_cell_with_options(d, lo, la, level_u8(l)?, allow_candidate)?,
                )),
                _ => None,
            });
        }
        finish_cells(out, rows)
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![
            sig(
                &[Varchar, Double, Double, Integer],
                struct_type(CELL_FIELDS),
            ),
            sig(
                &[Varchar, Double, Double, Integer, Boolean],
                struct_type(CELL_FIELDS),
            ),
        ]
    }
}

/// pack_int64(domain_id, level, x_idx, y_idx) -> BIGINT
pub struct PackInt64;
impl VScalar for PackInt64 {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (d, l, x, y) = (
            prims::<i32>(input, 0, n),
            prims::<i32>(input, 1, n),
            prims::<i64>(input, 2, n),
            prims::<i64>(input, 3, n),
        );
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(match (d[i], l[i], x[i], y[i]) {
                (Some(d), Some(l), Some(x), Some(y)) => {
                    let id = u16::try_from(d)
                        .map_err(|_| format!("domain_id must be in [1, 511]; received {d}"))?;
                    Some(packing::pack(
                        id,
                        Cell {
                            level: level_u8(l)?,
                            x_idx: index_u64(x, "x_idx")?,
                            y_idx: index_u64(y, "y_idx")?,
                        },
                    )?)
                }
                _ => None,
            });
        }
        finish_scalar(out, rows, put_i64);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[Integer, Integer, Bigint, Bigint], type_id(Bigint))]
    }
}

/// unpack_int64(packed) -> STRUCT(domain_id, level, x_idx, y_idx)
pub struct UnpackInt64;
impl VScalar for UnpackInt64 {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let p = prims::<i64>(input, 0, n);
        let mut valid = vec![false; n];
        let (mut d, mut l, mut x, mut y) = (vec![0; n], vec![0; n], vec![0; n], vec![0; n]);
        for i in 0..n {
            if let Some(v) = p[i] {
                let (id, c) = packing::unpack(v)?;
                valid[i] = true;
                d[i] = id as i32;
                l[i] = c.level as i32;
                x[i] = c.x_idx as i64;
                y[i] = c.y_idx as i64;
            }
        }
        finish_struct(
            out,
            &valid,
            vec![Field::I32(d), Field::I32(l), Field::I64(x), Field::I64(y)],
        );
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(
            &[Bigint],
            struct_type(&[
                ("domain_id", Integer),
                ("level", Integer),
                ("x_idx", Bigint),
                ("y_idx", Bigint),
            ]),
        )]
    }
}

/// canonical_to_gid(level, x_idx, y_idx) -> VARCHAR
pub struct CanonicalToGid;
impl VScalar for CanonicalToGid {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (l, x, y) = (
            prims::<i32>(input, 0, n),
            prims::<i64>(input, 1, n),
            prims::<i64>(input, 2, n),
        );
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(match (l[i], x[i], y[i]) {
                (Some(l), Some(x), Some(y)) => Some(codec::encode(Cell {
                    level: level_u8(l)?,
                    x_idx: index_u64(x, "x_idx")?,
                    y_idx: index_u64(y, "y_idx")?,
                })?),
                _ => None,
            });
        }
        finish_scalar(out, rows, |v, i, s: String| v.insert(i, s.as_str()));
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[Integer, Bigint, Bigint], type_id(Varchar))]
    }
}

/// packed_to_gid(packed_id) -> VARCHAR: the gid of a packed id (geosquare-v2 `unpack(packed)[1]`;
/// the name `unpack` is DuckDB syntax).
pub struct PackedToGid;
impl VScalar for PackedToGid {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let p = prims::<i64>(input, 0, n);
        let mut rows = Vec::with_capacity(n);
        for v in p.iter().take(n) {
            rows.push(match v {
                Some(v) => Some(codec::encode(packing::unpack(*v)?.1)?),
                None => None,
            });
        }
        finish_scalar(out, rows, |v, i, s: String| v.insert(i, s.as_str()));
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[Bigint], type_id(Varchar))]
    }
}

/// gid_to_canonical(domain_code, gid) -> STRUCT(domain_code, level, x_idx, y_idx)
pub struct GidToCanonical;
impl VScalar for GidToCanonical {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (d, g) = (domains(input, 0, n)?, strings(input, 1, n));
        let mut valid = vec![false; n];
        let (mut code, mut l, mut x, mut y) =
            (vec![String::new(); n], vec![0; n], vec![0; n], vec![0; n]);
        for i in 0..n {
            if let (Some(d), Some(g)) = (d[i], &g[i]) {
                let c = codec::decode(g)?;
                valid[i] = true;
                code[i] = d.code.to_string();
                l[i] = c.level as i32;
                x[i] = c.x_idx as i64;
                y[i] = c.y_idx as i64;
            }
        }
        finish_struct(
            out,
            &valid,
            vec![
                Field::Str(code),
                Field::I32(l),
                Field::I64(x),
                Field::I64(y),
            ],
        );
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(
            &[Varchar, Varchar],
            struct_type(&[
                ("domain_code", Varchar),
                ("level", Integer),
                ("x_idx", Bigint),
                ("y_idx", Bigint),
            ]),
        )]
    }
}

/// pack(domain, gid) -> BIGINT; domain is a domain_id INTEGER (engine) or a domain_code VARCHAR.
pub struct Pack;
impl VScalar for Pack {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (d, g) = (domains(input, 0, n)?, strings(input, 1, n));
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(match (d[i], &g[i]) {
                (Some(d), Some(g)) => Some(packing::pack(d.id, codec::decode(g)?)?),
                _ => None,
            });
        }
        finish_scalar(out, rows, put_i64);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![
            sig(&[Integer, Varchar], type_id(Bigint)),
            sig(&[Varchar, Varchar], type_id(Bigint)),
        ]
    }
}

/// parent(gid) -> VARCHAR (NULL for the level-0 cell)
pub struct Parent;
impl VScalar for Parent {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let mut rows = Vec::with_capacity(n);
        for g in strings(input, 0, n) {
            rows.push(match g {
                Some(g) => match topology::parent(codec::decode(&g)?) {
                    Some(p) => Some(codec::encode(p)?),
                    None => None,
                },
                None => None,
            });
        }
        finish_scalar(out, rows, |v, i, s: String| v.insert(i, s.as_str()));
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[Varchar], type_id(Varchar))]
    }
}

fn gids(cells: Vec<Cell>) -> Result<Vec<String>, Box<dyn Error>> {
    cells
        .into_iter()
        .map(|c| codec::encode(c).map_err(|e| e.into()))
        .collect()
}

/// children(gid) -> VARCHAR[]
pub struct Children;
impl VScalar for Children {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let mut rows = Vec::with_capacity(n);
        for g in strings(input, 0, n) {
            rows.push(match g {
                Some(g) => Some(gids(topology::children(codec::decode(&g)?)?)?),
                None => None,
            });
        }
        finish_list(out, rows);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[Varchar], LogicalTypeHandle::list(&type_id(Varchar)))]
    }
}

/// k_ring(gid, k) / k_disk(gid, k) -> VARCHAR[]
pub struct KRing;
pub struct KDisk;
fn k_cells(input: &mut DataChunkHandle, out: &mut dyn WritableVector, disk: bool) -> R {
    let n = input.len();
    let (g, k) = (strings(input, 0, n), prims::<i64>(input, 1, n));
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        rows.push(match (&g[i], k[i]) {
            (Some(g), Some(k)) => {
                let c = codec::decode(g)?;
                let k = index_u64(k, "k")?;
                Some(gids(if disk {
                    topology::k_disk(c, k)?
                } else {
                    topology::k_ring(c, k)?
                })?)
            }
            _ => None,
        });
    }
    finish_list(out, rows);
    Ok(())
}
impl VScalar for KRing {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        k_cells(input, out, false)
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(
            &[Varchar, Bigint],
            LogicalTypeHandle::list(&type_id(Varchar)),
        )]
    }
}
impl VScalar for KDisk {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        k_cells(input, out, true)
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(
            &[Varchar, Bigint],
            LogicalTypeHandle::list(&type_id(Varchar)),
        )]
    }
}

/// distance_m(gid_a, gid_b) -> DOUBLE (same-level grid distance in metres)
pub struct DistanceM;
impl VScalar for DistanceM {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (a, b) = (strings(input, 0, n), strings(input, 1, n));
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(match (&a[i], &b[i]) {
                (Some(a), Some(b)) => {
                    Some(topology::distance_m(codec::decode(a)?, codec::decode(b)?)?)
                }
                _ => None,
            });
        }
        finish_scalar(out, rows, put_f64);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[Varchar, Varchar], type_id(Double))]
    }
}

/// cell_distance(first_uri, second_uri) -> DOUBLE; both URIs must share a domain.
pub struct CellDistance;
impl VScalar for CellDistance {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (a, b) = (strings(input, 0, n), strings(input, 1, n));
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(match (&a[i], &b[i]) {
                (Some(a), Some(b)) => {
                    let ((da, ga), (db, gb)) = (parse_uri_str(a)?, parse_uri_str(b)?);
                    if da.id != db.id {
                        return Err(format!(
                            "cell_distance needs one domain; got {} and {}",
                            da.code, db.code
                        )
                        .into());
                    }
                    Some(topology::distance_m(
                        codec::decode(&ga)?,
                        codec::decode(&gb)?,
                    )?)
                }
                _ => None,
            });
        }
        finish_scalar(out, rows, put_f64);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[Varchar, Varchar], type_id(Double))]
    }
}

/// cell_side_m(level) -> DOUBLE
pub struct CellSideM;
impl VScalar for CellSideM {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let mut rows = Vec::with_capacity(n);
        for l in prims::<i32>(input, 0, n) {
            rows.push(match l {
                Some(l) => Some(codec::cell_side_m(level_u8(l)?)?),
                None => None,
            });
        }
        finish_scalar(out, rows, put_f64);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[Integer], type_id(Double))]
    }
}

/// descendant_range(domain, parent_gid, child_level) -> STRUCT(lo BIGINT, hi BIGINT) (spec 2.0, 8.1)
pub struct DescendantRange;
impl VScalar for DescendantRange {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (d, g, l) = (
            domains(input, 0, n)?,
            strings(input, 1, n),
            prims::<i32>(input, 2, n),
        );
        let mut valid = vec![false; n];
        let (mut lo, mut hi) = (vec![0; n], vec![0; n]);
        for i in 0..n {
            if let (Some(d), Some(g), Some(l)) = (d[i], &g[i], l[i]) {
                let (a, b) = topology::descendant_range(d.id, codec::decode(g)?, level_u8(l)?)?;
                valid[i] = true;
                lo[i] = a;
                hi[i] = b;
            }
        }
        finish_struct(out, &valid, vec![Field::I64(lo), Field::I64(hi)]);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        let t = || struct_type(&[("lo", Bigint), ("hi", Bigint)]);
        vec![
            sig(&[Integer, Varchar, Integer], t()),
            sig(&[Varchar, Varchar, Integer], t()),
        ]
    }
}

/// parse_uri(uri) -> STRUCT(domain_code, gid)
pub struct ParseUri;
impl VScalar for ParseUri {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let u = strings(input, 0, n);
        let mut valid = vec![false; n];
        let (mut code, mut gid) = (vec![String::new(); n], vec![String::new(); n]);
        for i in 0..n {
            if let Some(u) = &u[i] {
                let (d, g) = parse_uri_str(u)?;
                valid[i] = true;
                code[i] = d.code.to_string();
                gid[i] = g;
            }
        }
        finish_struct(out, &valid, vec![Field::Str(code), Field::Str(gid)]);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(
            &[Varchar],
            struct_type(&[("domain_code", Varchar), ("gid", Varchar)]),
        )]
    }
}

/// cell_to_geometry(domain_code, gid [, output_crs [, geometry_format [, allow_candidate_crs]]]) -> VARCHAR
/// `cell_to_geometry(packed_id, ...)` also accepts the same optional arguments.
/// `output_crs` is `grid` (default) or `EPSG:4326`.
/// `geometry_format` is `wkt` (default) or `geojson`.
/// Candidate CRS output requires the final boolean to be true.
pub struct CellToGeometry;

/// Outline of a cell as WKT or GeoJSON, in the grid CRS or in lon/lat.
fn cell_geometry(
    d: &'static domain::Domain,
    c: Cell,
    crs: &str,
    fmt: &str,
    allow_candidate_crs: bool,
) -> Result<String, Box<dyn Error>> {
    let ring: Vec<(f64, f64)> = match crs.trim().to_ascii_uppercase().as_str() {
        "GRID" | "PROJECTED" => {
            let b = geo::cell_bounds(d, c)?;
            vec![
                (b.min_x, b.min_y),
                (b.max_x, b.min_y),
                (b.max_x, b.max_y),
                (b.min_x, b.max_y),
            ]
        }
        "WGS84" | "EPSG:4326" | "OGC:CRS84" => {
            let mut r = geo::cell_polygon_wgs84_with_options(d, c, 25_000.0, allow_candidate_crs)?;
            r.pop();
            r
        }
        other => {
            return Err(
                format!("output_crs must be 'grid' or 'EPSG:4326'; received {other:?}").into(),
            )
        }
    };
    match fmt.trim().to_ascii_lowercase().as_str() {
        "wkt" => Ok(geom::to_wkt(&[ring])),
        "geojson" => Ok(geom::to_geojson(&[ring])),
        other => {
            Err(format!("geometry_format must be 'wkt' or 'geojson'; received {other:?}").into())
        }
    }
}

impl VScalar for CellToGeometry {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let ncols = input.num_columns();
        // cell_to_geometry(packed_id [, output_crs [, format [, allow_candidate_crs]]]): the domain is in the packed id.
        let by_packed = col_type(input, 0) == LogicalTypeId::Bigint;
        let first_opt = if by_packed { 1 } else { 2 };
        let candidate_col =
            (ncols > 0 && col_type(input, ncols - 1) == Boolean).then_some(ncols - 1);
        let argument_cols = candidate_col.unwrap_or(ncols);
        let allow_candidate = candidate_col
            .map(|col| prims::<bool>(input, col, n))
            .unwrap_or_else(|| vec![Some(false); n]);
        let crs = if argument_cols > first_opt {
            strings(input, first_opt, n)
        } else {
            vec![Some("grid".into()); n]
        };
        let fmt = if argument_cols > first_opt + 1 {
            strings(input, first_opt + 1, n)
        } else {
            vec![Some("wkt".into()); n]
        };
        let cells: Vec<Option<(&'static domain::Domain, Cell)>> = if by_packed {
            prims::<i64>(input, 0, n)
                .into_iter()
                .map(|p| {
                    p.map(|p| {
                        let (id, c) = packing::unpack(p)?;
                        Ok::<_, Box<dyn Error>>((crate::grid::domain_by_id(id as i32)?, c))
                    })
                    .transpose()
                })
                .collect::<Result<_, _>>()?
        } else {
            let (d, g) = (domains(input, 0, n)?, strings(input, 1, n));
            d.into_iter()
                .zip(g)
                .map(|(d, g)| match (d, g) {
                    (Some(d), Some(g)) => Ok(Some((d, codec::decode(&g)?))),
                    _ => Ok::<_, Box<dyn Error>>(None),
                })
                .collect::<Result<_, _>>()?
        };
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(match (cells[i], &crs[i], &fmt[i], allow_candidate[i]) {
                (Some((d, c)), Some(crs), Some(fmt), Some(allow_candidate)) => {
                    Some(cell_geometry(d, c, crs, fmt, allow_candidate)?)
                }
                _ => None,
            });
        }
        finish_scalar(out, rows, |v, i, s: String| v.insert(i, s.as_str()));
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![
            sig(&[Varchar, Varchar], type_id(Varchar)),
            sig(&[Varchar, Varchar, Varchar], type_id(Varchar)),
            sig(&[Varchar, Varchar, Varchar, Varchar], type_id(Varchar)),
            sig(&[Varchar, Varchar, Boolean], type_id(Varchar)),
            sig(&[Varchar, Varchar, Varchar, Boolean], type_id(Varchar)),
            sig(
                &[Varchar, Varchar, Varchar, Varchar, Boolean],
                type_id(Varchar),
            ),
            sig(&[Bigint], type_id(Varchar)),
            sig(&[Bigint, Varchar], type_id(Varchar)),
            sig(&[Bigint, Varchar, Varchar], type_id(Varchar)),
            sig(&[Bigint, Boolean], type_id(Varchar)),
            sig(&[Bigint, Varchar, Boolean], type_id(Varchar)),
            sig(&[Bigint, Varchar, Varchar, Boolean], type_id(Varchar)),
        ]
    }
}

/// verify_registry(database_path, signature_path, trust_path) -> BOOLEAN.
///
/// Returns true only after checking the SHA-256 digest, Ed25519 signature, and trusted key.
pub struct VerifyRegistry;
impl VScalar for VerifyRegistry {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len();
        let (database, signature, trust) = (
            strings(input, 0, n),
            strings(input, 1, n),
            strings(input, 2, n),
        );
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(match (&database[i], &signature[i], &trust[i]) {
                (Some(database), Some(signature), Some(trust)) => Some(
                    crate::registry::verify_paths(database, signature, trust)
                        .map(|_| true)
                        .map_err(Box::<dyn Error>::from)?,
                ),
                _ => None,
            });
        }
        finish_scalar(out, rows, put_bool);
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(
            &[Varchar, Varchar, Varchar],
            type_id(LogicalTypeId::Boolean),
        )]
    }
}

/// engine_version() -> VARCHAR
pub struct EngineVersion;
impl VScalar for EngineVersion {
    type State = ();
    fn invoke(_: &(), input: &mut DataChunkHandle, out: &mut dyn WritableVector) -> R {
        let n = input.len().max(1);
        let s = format!(
            "geosquare_v2 {} (spec 2.0, registry {}, profile {})",
            env!("CARGO_PKG_VERSION"),
            domain::REGISTRY_VERSION,
            domain::PROFILE_VERSION
        );
        let v = out.flat_vector();
        for i in 0..n {
            v.insert(i, s.as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![sig(&[], type_id(Varchar))]
    }
}

/// Register every scalar function.
///
/// Engine names that clash with DuckDB are not registered, so results never depend
/// on the DuckDB version: `encode`/`decode` are DuckDB BLOB built-ins (DuckDB 1.4
/// resolves `decode('<gid>')` to the built-in), and `UNPACK(...)` is DuckDB syntax.
/// Use the v2 names `canonical_to_gid`, `gid_to_canonical` and `unpack_int64`.
pub fn register(con: &Connection) -> Result<(), Box<dyn Error>> {
    con.register_scalar_function::<PointToCell>("point_to_cell")?;
    con.register_scalar_function::<PackInt64>("pack_int64")?;
    con.register_scalar_function::<UnpackInt64>("unpack_int64")?;
    con.register_scalar_function::<CanonicalToGid>("canonical_to_gid")?;
    con.register_scalar_function::<PackedToGid>("packed_to_gid")?;
    con.register_scalar_function::<GidToCanonical>("gid_to_canonical")?;
    con.register_scalar_function::<Pack>("pack")?;
    con.register_scalar_function::<Parent>("parent")?;
    con.register_scalar_function::<Children>("children")?;
    con.register_scalar_function::<KRing>("k_ring")?;
    con.register_scalar_function::<KDisk>("k_disk")?;
    con.register_scalar_function::<DistanceM>("distance_m")?;
    con.register_scalar_function::<CellDistance>("cell_distance")?;
    con.register_scalar_function::<CellSideM>("cell_side_m")?;
    con.register_scalar_function::<DescendantRange>("descendant_range")?;
    con.register_scalar_function::<ParseUri>("parse_uri")?;
    con.register_scalar_function::<CellToGeometry>("cell_to_geometry")?;
    con.register_scalar_function::<VerifyRegistry>("verify_registry")?;
    con.register_scalar_function::<EngineVersion>("geosquare_engine_version")?;
    Ok(())
}
