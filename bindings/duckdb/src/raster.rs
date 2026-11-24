//! `read_raster(path, bbox := [min_lon, min_lat, max_lon, max_lat])`: the pixels of a tiled
//! GeoTIFF / Cloud Optimized GeoTIFF in lon/lat (EPSG:4326) whose centre lies inside a box, one row per valid
//! pixel: `lon`, `lat` (pixel centre) and `value`. Only the tiles the box touches are read
//! (range requests on S3), so a raster of several GB is cheap to sample by area.
//!
//! To cells: `SELECT point_to_cell('ID', lon, lat, 12).packed_id, avg(value) ... GROUP BY 1`
//! (the mean of the pixels whose centre lies in the cell, as the hazard tables were built).
//!
//! Supported: classic TIFF and BigTIFF, one band, tiled, no compression or Deflate (zlib),
//! predictor none / horizontal / floating point, 8/16/32/64-bit integers and 32/64-bit
//! floats, GDAL_NODATA, GeoTIFF ModelPixelScale + ModelTiepoint in EPSG:4326.

use crate::{
    sql::{self, Pool},
    storage::{S3Params, Storage},
};
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId},
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab, Value},
};
use std::{error::Error, sync::Arc, sync::Mutex};

type E = Box<dyn Error>;
const CHUNK: usize = 2048;
const HEADER: u64 = 1 << 16;

pub struct Raster {
    path: String,
    store: Storage,
    width: u64,
    height: u64,
    tile_w: u64,
    tile_h: u64,
    offsets: Vec<u64>,
    counts: Vec<u64>,
    bits: u16,
    format: u16,
    predictor: u16,
    compression: u16,
    little: bool,
    x0: f64,
    y0: f64,
    sx: f64,
    sy: f64,
    nodata: Option<f64>,
}

struct Ifd {
    little: bool,
    big: bool,
    head: Vec<u8>,
}

impl Ifd {
    fn u16(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.little {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    }
    fn u32(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.little {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    }
    fn u64(&self, b: &[u8]) -> u64 {
        let a: [u8; 8] = b[..8].try_into().unwrap();
        if self.little {
            u64::from_le_bytes(a)
        } else {
            u64::from_be_bytes(a)
        }
    }
    fn f64(&self, b: &[u8]) -> f64 {
        f64::from_bits(self.u64(b))
    }
}

/// (type size in bytes) of TIFF field types.
fn type_size(t: u16) -> u64 {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => 1,
    }
}

impl Raster {
    fn bytes(&self, ifd: &Ifd, off: u64, len: u64) -> Result<Vec<u8>, E> {
        if off + len <= ifd.head.len() as u64 {
            return Ok(ifd.head[off as usize..(off + len) as usize].to_vec());
        }
        self.store.get_range(&self.path, off, len)
    }

    pub fn open(store: Storage, path: &str) -> Result<Raster, E> {
        let head = store.get_range(path, 0, HEADER)?;
        if head.len() < 16 {
            return Err(format!("{path}: not a TIFF file").into());
        }
        let little = match &head[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return Err(format!("{path}: not a TIFF file").into()),
        };
        let mut ifd = Ifd {
            little,
            big: false,
            head,
        };
        let magic = ifd.u16(&ifd.head[2..4]);
        ifd.big = match magic {
            42 => false,
            43 => true,
            _ => return Err(format!("{path}: not a TIFF file (magic {magic})").into()),
        };
        let mut r = Raster {
            path: path.to_string(),
            store,
            width: 0,
            height: 0,
            tile_w: 0,
            tile_h: 0,
            offsets: Vec::new(),
            counts: Vec::new(),
            bits: 0,
            format: 1,
            predictor: 1,
            compression: 1,
            little,
            x0: 0.0,
            y0: 0.0,
            sx: 0.0,
            sy: 0.0,
            nodata: None,
        };
        let first = if ifd.big {
            ifd.u64(&ifd.head[8..16])
        } else {
            ifd.u32(&ifd.head[4..8]) as u64
        };
        let (count_len, entry_len, inline) = if ifd.big {
            (8u64, 20u64, 8u64)
        } else {
            (2, 12, 4)
        };
        let cnt_bytes = r.bytes(&ifd, first, count_len)?;
        let n = if ifd.big {
            ifd.u64(&cnt_bytes)
        } else {
            ifd.u16(&cnt_bytes) as u64
        };
        let entries = r.bytes(&ifd, first + count_len, n * entry_len)?;
        let (mut scale, mut tie, mut geokeys) = (Vec::new(), Vec::new(), Vec::new());
        let mut spp = 1u16;
        for i in 0..n as usize {
            let e = &entries[i * entry_len as usize..(i + 1) * entry_len as usize];
            let tag = ifd.u16(&e[0..2]);
            let typ = ifd.u16(&e[2..4]);
            let count = if ifd.big {
                ifd.u64(&e[4..12])
            } else {
                ifd.u32(&e[4..8]) as u64
            };
            let vpos = if ifd.big { 12 } else { 8 };
            let size = type_size(typ) * count;
            let data = if size <= inline {
                e[vpos..vpos + size as usize].to_vec()
            } else {
                let off = if ifd.big {
                    ifd.u64(&e[vpos..vpos + 8])
                } else {
                    ifd.u32(&e[vpos..vpos + 4]) as u64
                };
                r.bytes(&ifd, off, size)?
            };
            let ints = |d: &[u8]| -> Vec<u64> {
                let w = type_size(typ) as usize;
                d.chunks(w)
                    .map(|c| match w {
                        1 => c[0] as u64,
                        2 => ifd.u16(c) as u64,
                        4 => ifd.u32(c) as u64,
                        _ => ifd.u64(c),
                    })
                    .collect()
            };
            let first_int = || ints(&data).first().copied().unwrap_or(0);
            match tag {
                256 => r.width = first_int(),
                257 => r.height = first_int(),
                258 => r.bits = first_int() as u16,
                259 => r.compression = first_int() as u16,
                277 => spp = first_int() as u16,
                317 => r.predictor = first_int() as u16,
                322 => r.tile_w = first_int(),
                323 => r.tile_h = first_int(),
                324 => r.offsets = ints(&data),
                325 => r.counts = ints(&data),
                339 => r.format = first_int() as u16,
                33550 => scale = data.chunks(8).map(|c| ifd.f64(c)).collect(),
                33922 => tie = data.chunks(8).map(|c| ifd.f64(c)).collect(),
                34735 => geokeys = ints(&data),
                42113 => {
                    let t = String::from_utf8_lossy(&data);
                    r.nodata = t.trim_matches(char::from(0)).trim().parse().ok();
                }
                _ => {}
            }
        }
        if r.tile_w == 0 || r.offsets.is_empty() {
            return Err(format!("{path}: read_raster needs a tiled GeoTIFF (COG); rewrite it with gdal_translate -of COG").into());
        }
        if spp != 1 {
            return Err(
                format!("{path}: {spp} bands; read_raster reads single-band rasters").into(),
            );
        }
        if !matches!(r.compression, 1 | 8 | 32946) {
            return Err(format!(
                "{path}: compression {} is not supported (none or Deflate)",
                r.compression
            )
            .into());
        }
        if scale.len() < 2 || tie.len() < 6 {
            return Err(format!("{path}: no GeoTIFF ModelPixelScale / ModelTiepoint").into());
        }
        // GeoKeyDirectory: header (4 shorts) then (key, location, count, value) entries.
        for k in geokeys.chunks(4).skip(1) {
            if k.len() == 4 && k[1] == 0 {
                if k[0] == 3072 {
                    return Err(format!(
                        "{path}: projected CRS (EPSG:{}); read_raster reads EPSG:4326 rasters",
                        k[3]
                    )
                    .into());
                }
                if k[0] == 2048 && k[3] != 4326 {
                    return Err(format!(
                        "{path}: geographic CRS EPSG:{}; read_raster reads EPSG:4326 rasters",
                        k[3]
                    )
                    .into());
                }
            }
        }
        r.sx = scale[0];
        r.sy = scale[1];
        r.x0 = tie[3] - tie[0] * r.sx;
        r.y0 = tie[4] + tie[1] * r.sy;
        Ok(r)
    }

    fn tiles_across(&self) -> u64 {
        self.width.div_ceil(self.tile_w)
    }

    /// Pixel window [c0, c1) x [r0, r1) of a lon/lat box, clipped to the raster.
    fn window(&self, b: [f64; 4]) -> (u64, u64, u64, u64) {
        let c0 = ((b[0] - self.x0) / self.sx).floor().max(0.0) as u64;
        let c1 = (((b[2] - self.x0) / self.sx).ceil().max(0.0) as u64).min(self.width);
        let r0 = ((self.y0 - b[3]) / self.sy).floor().max(0.0) as u64;
        let r1 = (((self.y0 - b[1]) / self.sy).ceil().max(0.0) as u64).min(self.height);
        (c0, c1.max(c0), r0, r1.max(r0))
    }

    /// Decoded samples of tile `t` (tile_w * tile_h values, row-major).
    fn tile(&self, t: usize) -> Result<Vec<f64>, E> {
        let (off, len) = (self.offsets[t], self.counts[t]);
        let n = (self.tile_w * self.tile_h) as usize;
        if len == 0 {
            return Ok(vec![f64::NAN; n]); // sparse tile
        }
        let raw = self.store.get_range(&self.path, off, len)?;
        let mut b = match self.compression {
            1 => raw,
            _ => miniz_oxide::inflate::decompress_to_vec_zlib(&raw)
                .map_err(|e| format!("{}: cannot inflate tile {t}: {e:?}", self.path))?,
        };
        let w = (self.bits / 8) as usize;
        let row_bytes = self.tile_w as usize * w;
        if b.len() < n * w {
            return Err(format!("{}: tile {t} is truncated", self.path).into());
        }
        match self.predictor {
            1 => {}
            2 => {
                // horizontal differencing on whole samples
                for row in b.chunks_mut(row_bytes) {
                    for i in 1..self.tile_w as usize {
                        let (prev, cur) = (i - 1, i);
                        match w {
                            1 => row[cur] = row[cur].wrapping_add(row[prev]),
                            2 => {
                                let p = self.u16(&row[prev * 2..]);
                                let c = self.u16(&row[cur * 2..]);
                                self.put_u16(&mut row[cur * 2..], c.wrapping_add(p));
                            }
                            4 => {
                                let p = self.u32(&row[prev * 4..]);
                                let c = self.u32(&row[cur * 4..]);
                                self.put_u32(&mut row[cur * 4..], c.wrapping_add(p));
                            }
                            _ => {
                                return Err("predictor 2 on 64-bit samples is not supported".into())
                            }
                        }
                    }
                }
            }
            3 => {
                // floating point: bytes differenced, then stored most significant byte first
                for row in b.chunks_mut(row_bytes) {
                    for i in 1..row.len() {
                        row[i] = row[i].wrapping_add(row[i - 1]);
                    }
                    let tmp = row.to_vec();
                    let cols = self.tile_w as usize;
                    for c in 0..cols {
                        for k in 0..w {
                            // byte k (big-endian order) of sample c
                            let v = tmp[k * cols + c];
                            let dst = if self.little {
                                c * w + (w - 1 - k)
                            } else {
                                c * w + k
                            };
                            row[dst] = v;
                        }
                    }
                }
            }
            p => return Err(format!("predictor {p} is not supported").into()),
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let c = &b[i * w..(i + 1) * w];
            out.push(match (self.format, w) {
                (3, 4) => f32::from_bits(self.u32(c)) as f64,
                (3, 8) => f64::from_bits(self.u64(c)),
                (2, 1) => c[0] as i8 as f64,
                (2, 2) => self.u16(c) as i16 as f64,
                (2, 4) => self.u32(c) as i32 as f64,
                (2, 8) => self.u64(c) as i64 as f64,
                (_, 1) => c[0] as f64,
                (_, 2) => self.u16(c) as f64,
                (_, 4) => self.u32(c) as f64,
                (_, 8) => self.u64(c) as f64,
                _ => {
                    return Err(format!(
                        "{}: {}-bit samples are not supported",
                        self.path, self.bits
                    )
                    .into())
                }
            });
        }
        Ok(out)
    }

    fn u16(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.little {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    }
    fn u32(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.little {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    }
    fn u64(&self, b: &[u8]) -> u64 {
        let a: [u8; 8] = b[..8].try_into().unwrap();
        if self.little {
            u64::from_le_bytes(a)
        } else {
            u64::from_be_bytes(a)
        }
    }
    fn put_u16(&self, b: &mut [u8], v: u16) {
        let a = if self.little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        b[..2].copy_from_slice(&a);
    }
    fn put_u32(&self, b: &mut [u8], v: u32) {
        let a = if self.little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        b[..4].copy_from_slice(&a);
    }
}

pub struct Bind {
    raster: Raster,
    window: (u64, u64, u64, u64),
    tiles: Vec<usize>,
    bbox: [f64; 4],
}

/// (next tile in the list, decoded current tile (index, samples), next row, next column).
type Cursor = (usize, Option<(usize, Vec<f64>)>, u64, u64);

/// Position in the tile list and the decoded current tile.
pub struct State {
    inner: Mutex<Cursor>,
}

fn named(bind: &BindInfo, name: &str) -> Option<Value> {
    bind.get_named_parameter(name).filter(|v| !v.is_null())
}

pub struct ReadRaster;
impl VTab for ReadRaster {
    type BindData = Bind;
    type InitData = State;

    fn bind(bind: &BindInfo) -> Result<Bind, E> {
        for c in ["lon", "lat", "value"] {
            bind.add_result_column(c, LogicalTypeId::Double.into());
        }
        let path = bind.get_parameter(0).to_string();
        let bbox: Vec<f64> = match named(bind, "bbox") {
            Some(v) => v
                .to_string()
                .trim_matches(['[', ']'])
                .split(',')
                .map(|x| x.trim().parse::<f64>())
                .collect::<Result<_, _>>()
                .map_err(|_| "bbox must be [min_lon, min_lat, max_lon, max_lat]")?,
            None => {
                return Err("read_raster needs bbox := [min_lon, min_lat, max_lon, max_lat]".into())
            }
        };
        if bbox.len() != 4 || !(bbox[0] < bbox[2] && bbox[1] < bbox[3]) {
            return Err("bbox must be [min_lon, min_lat, max_lon, max_lat] with min < max".into());
        }
        let max_pixels = named(bind, "max_pixels").map_or(200_000_000, |v| v.to_int64()) as u64;
        let s3 = S3Params {
            region: named(bind, "s3_region").map(|v| v.to_string()),
            endpoint: named(bind, "s3_endpoint").map(|v| v.to_string()),
            url_style: named(bind, "s3_url_style").map(|v| v.to_string()),
            use_ssl: None,
            access_key_id: named(bind, "s3_access_key_id").map(|v| v.to_string()),
            secret_access_key: named(bind, "s3_secret_access_key").map(|v| v.to_string()),
            session_token: named(bind, "s3_session_token").map(|v| v.to_string()),
        };
        let pool = unsafe { &*bind.get_extra_info::<Arc<Pool>>() }.clone();
        let store = sql::with_pool(&pool, || -> Result<Storage, E> {
            let lease = sql::lease()?;
            Storage::for_path(&lease, &path, &s3)
        })?;
        let raster = Raster::open(store, &path)?;
        let window = raster.window([bbox[0], bbox[1], bbox[2], bbox[3]]);
        let (c0, c1, r0, r1) = window;
        let pixels = (c1 - c0) * (r1 - r0);
        if pixels > max_pixels {
            return Err(format!(
                "the box covers {pixels} pixels, more than max_pixels := {max_pixels}; use a smaller bbox or raise max_pixels"
            )
            .into());
        }
        let across = raster.tiles_across();
        let mut tiles = Vec::new();
        if c1 > c0 && r1 > r0 {
            for ty in r0 / raster.tile_h..=(r1 - 1) / raster.tile_h {
                for tx in c0 / raster.tile_w..=(c1 - 1) / raster.tile_w {
                    tiles.push((ty * across + tx) as usize);
                }
            }
        }
        Ok(Bind {
            raster,
            window,
            tiles,
            bbox: [bbox[0], bbox[1], bbox[2], bbox[3]],
        })
    }

    fn init(_: &InitInfo) -> Result<State, E> {
        Ok(State {
            inner: Mutex::new((0, None, 0, 0)),
        })
    }

    fn func(f: &TableFunctionInfo<Self>, out: &mut DataChunkHandle) -> Result<(), E> {
        let b = f.get_bind_data();
        let st = f.get_init_data();
        let mut guard = st.inner.lock().map_err(|_| "poisoned")?;
        let g = &mut *guard;
        let r = &b.raster;
        let (c0, c1, r0, r1) = b.window;
        let across = r.tiles_across();
        let (mut lon, mut lat, mut val) = (
            Vec::with_capacity(CHUNK),
            Vec::with_capacity(CHUNK),
            Vec::with_capacity(CHUNK),
        );
        while lon.len() < CHUNK {
            if g.1.is_none() {
                if g.0 >= b.tiles.len() {
                    break;
                }
                let t = b.tiles[g.0];
                g.0 += 1;
                let data = r.tile(t)?;
                let (tx, ty) = (t as u64 % across, t as u64 / across);
                // first pixel of this tile inside the window
                g.2 = (ty * r.tile_h).max(r0);
                g.3 = (tx * r.tile_w).max(c0);
                g.1 = Some((t, data));
            }
            let (t, data) = g.1.as_ref().unwrap();
            let (tx, ty) = (*t as u64 % across, *t as u64 / across);
            let (col_lo, col_hi) = ((tx * r.tile_w).max(c0), ((tx + 1) * r.tile_w).min(c1));
            let row_hi = ((ty + 1) * r.tile_h).min(r1);
            let mut done = false;
            while lon.len() < CHUNK {
                let (row, col) = (g.2, g.3);
                if row >= row_hi {
                    done = true;
                    break;
                }
                let v = data[((row - ty * r.tile_h) * r.tile_w + (col - tx * r.tile_w)) as usize];
                let (x, y) = (
                    r.x0 + (col as f64 + 0.5) * r.sx,
                    r.y0 - (row as f64 + 0.5) * r.sy,
                );
                // The window is widened to whole pixels; only centres inside the box are returned.
                let [min_x, min_y, max_x, max_y] = b.bbox;
                let inside = x >= min_x && x <= max_x && y >= min_y && y <= max_y;
                if inside && v.is_finite() && r.nodata.is_none_or(|nd| v != nd) {
                    lon.push(x);
                    lat.push(y);
                    val.push(v);
                }
                if col + 1 < col_hi {
                    g.3 = col + 1;
                } else {
                    g.3 = col_lo;
                    g.2 = row + 1;
                }
            }
            if done {
                g.1 = None;
            }
        }
        let n = lon.len();
        for (k, v) in [lon, lat, val].into_iter().enumerate() {
            let mut vec = out.flat_vector(k);
            unsafe { vec.as_mut_slice::<f64>()[..n].copy_from_slice(&v) };
        }
        out.set_len(n);
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![LogicalTypeId::Varchar.into()])
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        let mut v: Vec<(String, LogicalTypeHandle)> = vec![
            (
                "bbox".into(),
                LogicalTypeHandle::list(&LogicalTypeId::Double.into()),
            ),
            ("max_pixels".into(), LogicalTypeId::Bigint.into()),
        ];
        for n in [
            "s3_region",
            "s3_endpoint",
            "s3_url_style",
            "s3_access_key_id",
            "s3_secret_access_key",
            "s3_session_token",
        ] {
            v.push((n.into(), LogicalTypeId::Varchar.into()));
        }
        Some(v)
    }
}
