//! Cell datasets (geosquare-v2 cell dataset contract) in three layouts:
//!
//! * `flat`        — `<root>/manifest.json` + `<root>/data/*.parquet`
//! * `partitioned` — `<root>/manifest.json` + `<root>/data/gid_prefix=<L chars>/*.parquet`
//!   (spec 2.0 section 15; `storage.partitioning` holds `gid_prefix:<L>`)
//! * `iceberg`     — an Iceberg table at `<root>` (`<root>/metadata/*.metadata.json`).
//!   The newest metadata is used automatically; the cell manifest lives in the table
//!   property `geosquare.manifest`.
//!
//! Reading runs inside DuckDB (read_parquet / iceberg_scan), so local paths, s3://,
//! http(s):// and anything else DuckDB can open work the same way. Plain HTTP cannot list
//! folders, so over http(s) the reader uses fixed names instead of globs: `manifest.json`,
//! `data/part-00000.parquet`, the partition index `gid_prefixes.json` written next to the
//! manifest, and `metadata/version-hint.text` for Iceberg.

use crate::{
    geom, polyfill,
    sql::{self, lit, Lease, Param, Params, Plan, E},
};
use geosquare_core::{codec, domain::Domain, packing, Cell};
use serde_json::Value as Json;

pub const MANIFEST_FILE: &str = "manifest.json";
/// Partition index of a gid_prefix dataset, for readers that cannot list folders.
pub const PREFIX_INDEX_FILE: &str = "gid_prefixes.json";
pub const MANIFEST_KEY: &str = "geosquare.manifest";

#[derive(Debug, Clone)]
pub enum Layout {
    Flat,
    Partitioned(usize),
    Iceberg { metadata: String },
}

pub struct Source {
    pub root: String,
    pub layout: Layout,
    pub manifest: Option<Json>,
}

pub fn join(root: &str, rest: &str) -> String {
    format!(
        "{}/{}",
        root.trim_end_matches('/'),
        rest.trim_start_matches('/')
    )
}

/// Plain HTTP(S) has no directory listing.
pub fn is_http(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.starts_with("http://") || p.starts_with("https://")
}

fn glob(lease: &Lease, pattern: &str) -> Result<Vec<String>, E> {
    match sql::query_text_on(
        lease,
        &format!("SELECT file FROM glob({}) ORDER BY file", lit(pattern)),
    ) {
        Ok(rows) => Ok(rows
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect()),
        // Object stores answer a missing exact key with 404 instead of an empty listing.
        Err(e) if is_not_found(&e.to_string()) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

fn is_not_found(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("404")
        || m.contains("not found")
        || m.contains("no such file")
        || m.contains("no files found")
}

fn read_text(lease: &Lease, path: &str) -> Result<String, E> {
    sql::query_text_on(
        lease,
        &format!("SELECT content FROM read_text({})", lit(path)),
    )?
    .into_iter()
    .next()
    .and_then(|r| r.into_iter().next().flatten())
    .ok_or_else(|| format!("{path}: not found").into())
}

/// Version number in an Iceberg metadata file name: `00012-<uuid>.metadata.json` or `v12.metadata.json`.
fn metadata_version(file: &str) -> Option<u64> {
    let name = file.rsplit('/').next()?;
    let stem = name.strip_suffix(".metadata.json")?;
    let digits: String = stem
        .trim_start_matches('v')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// The newest metadata file of an Iceberg table at `root`.
///
/// `version-hint.text` wins when present. Otherwise the three files with the highest
/// version numbers are read and the one with the largest `last-updated-ms` is used, so a
/// stale high-numbered file cannot shadow the real head.
pub fn latest_iceberg_metadata(lease: &Lease, root: &str) -> Result<(String, Json), E> {
    if is_http(root) {
        let hint = read_text(lease, &join(root, "metadata/version-hint.text")).map_err(|_| {
            format!("{root}: over HTTP an Iceberg table needs metadata/version-hint.text")
        })?;
        let hint = hint.trim();
        for cand in [
            format!("metadata/{hint}.metadata.json"),
            format!("metadata/v{hint}.metadata.json"),
        ] {
            let f = join(root, &cand);
            if let Ok(text) = read_text(lease, &f) {
                return Ok((f, serde_json::from_str(&text)?));
            }
        }
        return Err(format!("{root}: version-hint.text points to a missing metadata file").into());
    }
    let files = glob(lease, &join(root, "metadata/*.metadata.json"))?;
    if files.is_empty() {
        return Err(format!("no Iceberg metadata under {}", join(root, "metadata")).into());
    }
    if let Ok(hint) = read_text(lease, &join(root, "metadata/version-hint.text")) {
        let hint = hint.trim();
        for cand in [
            format!("{hint}.metadata.json"),
            format!("v{hint}.metadata.json"),
        ] {
            if let Some(f) = files.iter().find(|f| f.ends_with(&format!("/{cand}"))) {
                let j: Json = serde_json::from_str(&read_text(lease, f)?)?;
                return Ok((f.clone(), j));
            }
        }
    }
    let mut ranked: Vec<&String> = files.iter().collect();
    ranked.sort_by_key(|f| std::cmp::Reverse(metadata_version(f).unwrap_or(0)));
    let mut best: Option<(i64, String, Json)> = None;
    for f in ranked.into_iter().take(3) {
        let j: Json = serde_json::from_str(&read_text(lease, f)?)?;
        let ts = j.get("last-updated-ms").and_then(Json::as_i64).unwrap_or(0);
        if best.as_ref().is_none_or(|(b, _, _)| ts > *b) {
            best = Some((ts, f.clone(), j));
        }
    }
    let (_, f, j) = best.ok_or("no readable Iceberg metadata")?;
    Ok((f, j))
}

/// Parsed Iceberg metadata file, cached by path (metadata files are never rewritten).
pub fn metadata_json(lease: &Lease, path: &str) -> Result<Json, E> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static C: OnceLock<Mutex<HashMap<String, Json>>> = OnceLock::new();
    let c = C.get_or_init(Default::default);
    if let Some(j) = c.lock().unwrap().get(path) {
        return Ok(j.clone());
    }
    let j: Json = serde_json::from_str(&read_text(lease, path)?)?;
    c.lock().unwrap().insert(path.to_string(), j.clone());
    Ok(j)
}

/// The metadata file named by `metadata/version-hint.text`, when the hint exists.
fn hinted_metadata(lease: &Lease, root: &str) -> Option<(String, Json)> {
    let hint = read_text(lease, &join(root, "metadata/version-hint.text")).ok()?;
    let hint = hint.trim();
    [
        format!("metadata/{hint}.metadata.json"),
        format!("metadata/v{hint}.metadata.json"),
    ]
    .iter()
    .find_map(|cand| {
        let f = join(root, cand);
        metadata_json(lease, &f).ok().map(|j| (f, j))
    })
}

fn partition_level(manifest: &Json) -> Option<usize> {
    manifest
        .pointer("/storage/partitioning")?
        .as_array()?
        .iter()
        .find_map(|p| p.as_str()?.strip_prefix("gid_prefix:")?.parse().ok())
}

/// Detect the layout of the dataset at `path`.
pub fn open(lease: &Lease, path: &str) -> Result<Source, E> {
    let path = path.trim_end_matches('/').to_string();
    if path.ends_with(".metadata.json") {
        let j = metadata_json(lease, &path)?;
        let root = j
            .get("location")
            .and_then(Json::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| path.rsplitn(3, '/').nth(2).unwrap_or("").to_string());
        let manifest = iceberg_manifest(&j)?;
        return Ok(Source {
            root,
            layout: Layout::Iceberg { metadata: path },
            manifest,
        });
    }
    // An Iceberg table with a version hint: two small reads, no listing.
    if let Some((metadata, j)) = hinted_metadata(lease, &path) {
        let manifest = iceberg_manifest(&j)?;
        return Ok(Source {
            root: path,
            layout: Layout::Iceberg { metadata },
            manifest,
        });
    }
    // Read the manifest directly: globbing an exact S3 key returns it even when it is absent.
    let manifest_text = match read_text(lease, &join(&path, MANIFEST_FILE)) {
        Ok(t) => Some(t),
        Err(e) if is_http(&path) || is_not_found(&e.to_string()) => None,
        Err(e) => return Err(e),
    };
    if let Some(text) = manifest_text {
        let manifest: Json = serde_json::from_str(&text)?;
        let layout = match partition_level(&manifest) {
            Some(l) => Layout::Partitioned(l),
            None => Layout::Flat,
        };
        return Ok(Source {
            root: path,
            layout,
            manifest: Some(manifest),
        });
    }
    if is_http(&path) || !glob(lease, &join(&path, "metadata/*.metadata.json"))?.is_empty() {
        let (metadata, j) = latest_iceberg_metadata(lease, &path)?;
        let manifest = iceberg_manifest(&j)?;
        return Ok(Source {
            root: path,
            layout: Layout::Iceberg { metadata },
            manifest,
        });
    }
    Err(format!("{path}: no {MANIFEST_FILE} and no Iceberg metadata found").into())
}

fn iceberg_manifest(metadata: &Json) -> Result<Option<Json>, E> {
    match metadata
        .pointer(&format!("/properties/{}", MANIFEST_KEY.replace('/', "~1")))
        .and_then(Json::as_str)
    {
        Some(s) => Ok(Some(serde_json::from_str(s)?)),
        None => Ok(None),
    }
}

/// Make sure the iceberg extension is loaded on the inner connection.
pub fn load_iceberg_ext(lease: &Lease) -> Result<(), E> {
    if sql::exec_on(lease, "LOAD iceberg").is_ok() {
        return Ok(());
    }
    sql::exec_on(lease, "INSTALL iceberg")
        .and_then(|_| sql::exec_on(lease, "LOAD iceberg"))
        .map_err(|e| {
            format!("the iceberg extension is needed to read Iceberg datasets: {e}").into()
        })
}

/// Every partition file of a gid_prefix dataset (listing, or the index over HTTP).
fn partition_files(lease: &Lease, src: &Source) -> Result<Vec<String>, E> {
    if !is_http(&src.root) {
        return glob(lease, &join(&src.root, "data/*/*.parquet"));
    }
    let index: Json = serde_json::from_str(
        &read_text(lease, &join(&src.root, PREFIX_INDEX_FILE)).map_err(|_| {
            format!(
                "{}: over HTTP a partitioned dataset needs {PREFIX_INDEX_FILE}",
                src.root
            )
        })?,
    )?;
    Ok(index
        .get("gid_prefixes")
        .and_then(Json::as_array)
        .ok_or("gid_prefixes.json has no gid_prefixes array")?
        .iter()
        .filter_map(Json::as_str)
        .map(|p| {
            join(
                &src.root,
                &format!("data/gid_prefix={p}/part-00000.parquet"),
            )
        })
        .collect())
}

/// FROM-clause for the whole dataset (or for selected partition files).
fn from_clause(lease: &Lease, src: &Source, files: Option<&[String]>) -> Result<String, E> {
    if let (Layout::Partitioned(_), None, true) = (&src.layout, files, is_http(&src.root)) {
        let all = partition_files(lease, src)?;
        return from_clause(lease, src, Some(&all));
    }
    Ok(match &src.layout {
        Layout::Flat if is_http(&src.root) => format!(
            "read_parquet({})",
            lit(&join(&src.root, "data/part-00000.parquet"))
        ),
        Layout::Flat => format!("read_parquet({})", lit(&join(&src.root, "data/*.parquet"))),
        Layout::Partitioned(_) => match files {
            Some(list) => format!(
                "read_parquet([{}], hive_partitioning = false)",
                list.iter().map(|f| lit(f)).collect::<Vec<_>>().join(", ")
            ),
            None => format!(
                "read_parquet({}, hive_partitioning = false)",
                lit(&join(&src.root, "data/*/*.parquet"))
            ),
        },
        Layout::Iceberg { metadata } => {
            load_iceberg_ext(lease)?;
            // Scan the table folder at the chosen metadata version with allow_moved_paths,
            // so a table copied away from where it was written (local -> S3/CDN) still
            // resolves its manifests and data files under the new folder.
            match metadata.rsplit_once("/metadata/") {
                Some((dir, file)) => format!(
                    "iceberg_scan({}, version = {}, allow_moved_paths = true)",
                    lit(dir),
                    lit(file.strip_suffix(".metadata.json").unwrap_or(file))
                ),
                None => format!("iceberg_scan({})", lit(metadata)),
            }
        }
    })
}

/// Identity of the dataset: domain and level from the manifest, else from parameters.
fn identity(src: &Source, p: &Params) -> Result<(&'static Domain, u8), E> {
    let code = p
        .text("domain_code")
        .or_else(|| {
            src.manifest
                .as_ref()?
                .get("domain_code")?
                .as_str()
                .map(str::to_string)
        })
        .ok_or("domain_code is unknown: the dataset has no manifest, pass domain_code := ...")?;
    let level = p
        .int("level")
        .or_else(|| src.manifest.as_ref()?.get("level")?.as_i64())
        .ok_or("level is unknown: the dataset has no manifest, pass level := ...")?;
    let d = crate::grid::domain_by_code(&code)?;
    if let Some(m) = &src.manifest {
        if let Some(pv) = m.get("profile_version").and_then(Json::as_str) {
            if pv != geosquare_core::domain::PROFILE_VERSION {
                return Err(format!(
                    "dataset profile_version {pv:?} does not match the engine profile {:?}",
                    geosquare_core::domain::PROFILE_VERSION
                )
                .into());
            }
        }
        if let Some(id) = m.get("domain_id").and_then(Json::as_i64) {
            if id != d.id as i64 {
                return Err(
                    format!("manifest domain_id {id} does not match domain {}", d.code).into(),
                );
            }
        }
    }
    Ok((d, crate::grid::level_u8(level as i32)?))
}

fn columns(lease: &Lease, from: &str) -> Result<Vec<String>, E> {
    Ok(sql::query_text_on(
        lease,
        &format!("SELECT column_name FROM (DESCRIBE SELECT * FROM {from})"),
    )?
    .into_iter()
    .filter_map(|r| r.into_iter().next().flatten())
    .collect())
}

/// read_cell_dataset(path, domain_code := NULL, level := NULL)
/// `with_geometry := 'wkt' | 'geojson'` (and `geometry_crs`, default EPSG:4326): add each
/// cell's outline as a `cell_geometry` column.
fn add_geometry(plan: Plan, p: &Params, domain_code: Option<&str>) -> Result<Plan, E> {
    let Some(format) = p.text("with_geometry") else {
        return Ok(plan);
    };
    let format = format.to_ascii_lowercase();
    if !matches!(format.as_str(), "wkt" | "geojson") {
        return Err(
            format!("with_geometry must be 'wkt' or 'geojson'; received {format:?}").into(),
        );
    }
    let crs = p.text("geometry_crs").unwrap_or_else(|| "EPSG:4326".into());
    let code = domain_code.ok_or("with_geometry needs the domain: pass domain_code := '<code>' (the dataset has no cell manifest)")?;
    let expr = if p.int("allow_candidate_crs").unwrap_or(0) != 0 {
        format!(
            "cell_to_geometry({}, gid, {}, {}, true)",
            lit(code),
            lit(&crs),
            lit(&format)
        )
    } else {
        format!(
            "cell_to_geometry({}, gid, {}, {})",
            lit(code),
            lit(&crs),
            lit(&format)
        )
    };
    Ok(match plan {
        Plan::Query(sql) => Plan::Query(format!(
            "SELECT *, {expr} AS cell_geometry FROM ({sql}) AS gs_g"
        )),
        Plan::Typed(sql, probe) => Plan::Typed(
            format!("SELECT *, {expr} AS cell_geometry FROM ({sql}) AS gs_g"),
            format!("SELECT *, NULL::VARCHAR AS cell_geometry FROM ({probe}) AS gs_p"),
        ),
        other => other,
    })
}

pub fn plan_read(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let lease = sql::lease()?;
    let src = open(&lease, &path)?;
    let code = if src.manifest.is_some() || p.text("domain_code").is_some() {
        Some(identity(&src, p)?.0.code) // also validates the manifest against the engine
    } else {
        None
    };
    let plan = Plan::Query(format!(
        "SELECT * FROM {}",
        from_clause(&lease, &src, None)?
    ));
    add_geometry(plan, p, code)
}

/// Query polygon in the grid CRS and its source CRS for `polygon_to_cells`.
fn query_polygon(p: &Params, d: &Domain, level: u8) -> Result<(String, String, f64), E> {
    let min_cov = p.float("min_coverage").unwrap_or(0.0);
    if let Some(wkt) = p.text("geometry") {
        // Any WKT geometry; plan_query dispatches on its type.
        let crs = p.text("source_crs").unwrap_or_else(|| "EPSG:4326".into());
        return Ok((wkt, crs, min_cov));
    }
    if let Some(Param::List(b)) = p.get("bbox") {
        let allow_candidate = p.int("allow_candidate_crs").unwrap_or(0) != 0;
        if !d.crs_released() && !allow_candidate {
            return Err(format!(
                "domain {} is not released for lon/lat: pass allow_candidate_crs := true for technical use",
                d.code
            )
            .into());
        }
        let v: Vec<f64> = b.iter().filter_map(Param::float).collect();
        if v.len() != 4 {
            return Err("bbox must be [min_lon, min_lat, max_lon, max_lat]".into());
        }
        let (x0, y0, x1, y1) = (v[0], v[1], v[2], v[3]);
        if !(x0 < x1 && y0 < y1) {
            return Err("bbox must have min < max (antimeridian bboxes are not supported)".into());
        }
        // Like geosquare-v2 query_cell_dataset: project the four corners, take their envelope,
        // and return every cell that overlaps it.
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for (lon, lat) in [(x0, y0), (x0, y1), (x1, y0), (x1, y1)] {
            let (x, y) = d
                .grid_crs
                .forward(lon, lat)
                .ok_or("bbox corner cannot be projected")?;
            xs.push(x);
            ys.push(y);
        }
        let (a, b) = (
            xs.iter().cloned().fold(f64::MAX, f64::min),
            xs.iter().cloned().fold(f64::MIN, f64::max),
        );
        let (c, e) = (
            ys.iter().cloned().fold(f64::MAX, f64::min),
            ys.iter().cloned().fold(f64::MIN, f64::max),
        );
        let wkt = format!("POLYGON (({a} {c}, {b} {c}, {b} {e}, {a} {e}, {a} {c}))");
        let _ = level;
        return Ok((wkt, "grid".into(), 0.0));
    }
    Err("pass geometry := '<WKT>' or bbox := [min_lon, min_lat, max_lon, max_lat]".into())
}

/// query_cell_dataset(path, geometry := WKT | bbox := [..], min_coverage, source_crs,
///   coverage_mode, max_candidate_limit, domain_code, level)
///
/// `geometry` may be a polygon, a line or points (WKT, lon/lat unless `source_crs`):
/// * POLYGON / MULTIPOLYGON → rows of the covered cells + `coverage_ratio`
/// * LINESTRING / MULTILINESTRING → rows of the crossed cells + `length_ratio`
/// * POINT / MULTIPOINT → rows of the cells holding the points + `point_count`
///
/// `bbox` behaves like a polygon (every cell overlapping the projected envelope).
pub fn plan_query(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let (plan, code) = plan_query_cells(p, &path)?;
    add_geometry(plan, p, Some(code))
}

/// The cells of the query area joined with the dataset, and the dataset's domain code.
fn plan_query_cells(p: &Params, path: &str) -> Result<(Plan, &'static str), E> {
    let path = path.to_string();
    let lease = sql::lease()?;
    let src = open(&lease, &path)?;
    let (d, level) = identity(&src, p)?;
    let limit = p.int("max_candidate_limit").unwrap_or(100_000);
    let (wkt, crs, min_cov) = query_polygon(p, d, level)?;
    let shape = geom::parse_geometry(&wkt)?;

    // Plan in Rust: the cells decide which partitions and which packed_id range to read,
    // and `keys` is the matching cell table for the join inside DuckDB.
    let (cells, keys, extra, extra_type): (Vec<Cell>, String, &str, &str) = match shape {
        geom::Geometry::Polygons(polys) => {
            let allow_candidate = p.int("allow_candidate_crs").unwrap_or(0) != 0;
            let opts = polyfill::PolyfillOptions {
                source_crs: crs.clone(),
                coverage_mode: p
                    .text("coverage_mode")
                    .unwrap_or_else(|| "GRID_PLANAR".into()),
                min_coverage: min_cov,
                max_candidate_limit: limit,
                allow_candidate_crs: allow_candidate,
            };
            let cells = polyfill::polyfill(d, polys, level, &opts)?;
            let keys = format!(
                "polygon_to_cells({}, {}, {}, source_crs := {}, coverage_mode := {}, min_coverage := {}, max_candidate_limit := {}, allow_candidate_crs := {})",
                lit(d.code), lit(&wkt), level, lit(&crs), lit(&opts.coverage_mode), opts.min_coverage, limit, allow_candidate
            );
            (
                cells.into_iter().map(|(c, _)| c).collect(),
                keys,
                "coverage_ratio",
                "DOUBLE",
            )
        }
        geom::Geometry::Lines(lines) => {
            let cells = crate::lines::line_to_cells(d, &lines, level, &crs, limit)?;
            let keys = format!(
                "line_to_cells({}, {}, {}, source_crs := {}, max_candidate_limit := {})",
                lit(d.code),
                lit(&wkt),
                level,
                lit(&crs),
                limit
            );
            (
                cells.into_iter().map(|(c, _)| c).collect(),
                keys,
                "length_ratio",
                "DOUBLE",
            )
        }
        geom::Geometry::Points(points) => {
            let cells = crate::lines::points_to_cells(d, &points, level, &crs)?;
            let rows: Vec<String> = cells
                .iter()
                .map(|(c, n)| {
                    Ok(format!(
                        "({}, {}, {n})",
                        packing::pack(d.id, *c)?,
                        lit(&codec::encode(*c)?)
                    ))
                })
                .collect::<Result<_, E>>()?;
            let keys = if rows.is_empty() {
                "(SELECT NULL::BIGINT AS packed_id, NULL::VARCHAR AS gid, NULL::BIGINT AS point_count LIMIT 0)".to_string()
            } else {
                format!(
                    "(SELECT * FROM (VALUES {}) AS v(packed_id, gid, point_count))",
                    rows.join(", ")
                )
            };
            (
                cells.into_iter().map(|(c, _)| c).collect(),
                keys,
                "point_count",
                "BIGINT",
            )
        }
    };
    let packed: Vec<i64> = cells
        .iter()
        .map(|c| packing::pack(d.id, *c))
        .collect::<Result<_, _>>()?;
    let (lo, hi) = (
        packed.iter().min().copied().unwrap_or(0),
        packed.iter().max().copied().unwrap_or(-1),
    );

    // Iceberg: choose data files from the cached manifests, read them with read_parquet.
    if let Layout::Iceberg { metadata } = &src.layout {
        let meta = metadata_json(&lease, metadata)?;
        if let Some(t) = crate::icefast::load(&lease, &src.root, metadata, &meta)? {
            let mut sorted = packed.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if let Some(sel) = t.files_for(&lease, &sorted)? {
                if sel.is_empty() {
                    return Ok((
                        Plan::Query(format!(
                            "SELECT {}, NULL::{extra_type} AS {extra} LIMIT 0",
                            t.empty_select()
                        )),
                        d.code,
                    ));
                }
                let list: Vec<String> = sel.iter().map(|f| lit(f)).collect();
                // Every column is cast to the type the probe declares: the proxy copies values
                // by the declared type, so they must match exactly (e.g. point_count BIGINT).
                let cols: Vec<String> = t
                    .columns
                    .iter()
                    .map(|(n, ty)| format!("CAST(d.{0} AS {ty}) AS {0}", sql::ident(n)))
                    .collect();
                return Ok((Plan::Typed(
                    format!(
                        "SELECT {}, CAST(k.{extra} AS {extra_type}) AS {extra} FROM read_parquet([{}], hive_partitioning = false) AS d \
                         JOIN {keys} AS k ON d.packed_id = k.packed_id WHERE d.packed_id BETWEEN {lo} AND {hi}",
                        cols.join(", "),
                        list.join(", ")
                    ),
                    format!("SELECT {}, NULL::{extra_type} AS {extra}", t.empty_select()),
                ), d.code));
            }
        }
    }
    let files = match &src.layout {
        Layout::Partitioned(l) => {
            let mut prefixes: Vec<String> = cells
                .iter()
                .map(|c| codec::encode(*c).map(|g| g.chars().take(*l).collect()))
                .collect::<Result<_, _>>()?;
            prefixes.sort();
            prefixes.dedup();
            let all = partition_files(&lease, &src)?;
            Some(
                all.into_iter()
                    .filter(|f| {
                        prefixes
                            .iter()
                            .any(|p| f.contains(&format!("/gid_prefix={p}/")))
                    })
                    .collect::<Vec<_>>(),
            )
        }
        _ => None,
    };
    let full = from_clause(&lease, &src, None)?;
    let from = match &files {
        Some(f) if f.is_empty() => {
            // No partition intersects the area: same columns, no rows.
            return Ok((
                Plan::Query(format!(
                    "SELECT *, NULL::{extra_type} AS {extra} FROM {full} LIMIT 0"
                )),
                d.code,
            ));
        }
        Some(f) => from_clause(&lease, &src, Some(f))?,
        None => full,
    };
    let cols = columns(&lease, &from)?;
    let sql = if cols.iter().any(|c| c == "packed_id") {
        format!(
            "SELECT d.*, k.{extra} FROM {from} AS d JOIN {keys} AS k ON d.packed_id = k.packed_id \
             WHERE d.packed_id BETWEEN {lo} AND {hi}"
        )
    } else if cols.iter().any(|c| c == "gid") {
        format!("SELECT d.*, k.{extra} FROM {from} AS d JOIN {keys} AS k ON d.gid = k.gid")
    } else {
        return Err("dataset has neither a packed_id nor a gid column".into());
    };
    Ok((Plan::Query(sql), d.code))
}

// ---------------------------------------------------------------------------
// cell_dataset_info / cell_dataset_columns: the metadata of a dataset as rows
// ---------------------------------------------------------------------------

/// (cell manifest, dataset_info) of a dataset.
fn describe_dataset(p: &Params) -> Result<(Source, Json, Json), E> {
    let path = p.pos_text(0, "path")?;
    let lease = sql::lease()?;
    let src = open(&lease, &path)?;
    let manifest = src.manifest.clone().unwrap_or(Json::Null);
    let info: Json = match &src.layout {
        Layout::Iceberg { metadata } => metadata_json(&lease, metadata)?
            .pointer(&format!(
                "/properties/{}",
                crate::metadata::INFO_KEY.replace('/', "~1")
            ))
            .and_then(Json::as_str)
            .and_then(|t| serde_json::from_str(t).ok()),
        _ => read_text(&lease, &join(&src.root, crate::metadata::INFO_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok()),
    }
    .unwrap_or(Json::Null);
    Ok((src, manifest, info))
}

fn jlit(v: &Json) -> String {
    format!("{}::JSON", lit(&v.to_string()))
}

fn slit(v: Option<&Json>) -> String {
    match v.and_then(Json::as_str) {
        Some(s) => lit(s),
        None => "NULL::VARCHAR".into(),
    }
}

/// cell_dataset_info(path): one row with the dataset's identity and descriptive metadata.
pub fn plan_info(p: &Params) -> Result<Plan, E> {
    let (src, m, info) = describe_dataset(p)?;
    let layout = match src.layout {
        Layout::Flat => "flat",
        Layout::Partitioned(_) => "partitioned",
        Layout::Iceberg { .. } => "iceberg",
    };
    let int = |v: Option<&Json>| {
        v.and_then(Json::as_i64)
            .map_or("NULL::BIGINT".into(), |n| format!("{n}::BIGINT"))
    };
    Ok(Plan::Query(format!(
        "SELECT {} AS path, {} AS layout, {} AS domain_code, {} AS level, {} AS row_count, \
         {} AS title, {} AS description, {} AS data_period, {} AS data_version, \
         {} AS source, {} AS coverage, {} AS provenance, {} AS manifest, {} AS dataset_info",
        lit(&src.root),
        lit(layout),
        slit(m.get("domain_code")),
        int(m.get("level")),
        int(m.pointer("/storage/row_count")),
        slit(info.get("title")),
        slit(info.get("description")),
        slit(info.get("data_period")),
        slit(info.get("data_version")),
        jlit(m.get("source").unwrap_or(&Json::Null)),
        jlit(m.get("coverage").unwrap_or(&Json::Null)),
        jlit(m.get("provenance").unwrap_or(&Json::Null)),
        jlit(&m),
        jlit(&info),
    )))
}

/// cell_dataset_columns(path): one row per value column of the cell manifest.
pub fn plan_columns(p: &Params) -> Result<Plan, E> {
    let (_, m, info) = describe_dataset(p)?;
    let fields = m
        .get("value_fields")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    if fields.is_empty() {
        return Ok(Plan::Query("SELECT NULL::VARCHAR AS \"column\", NULL::VARCHAR AS parquet_type, NULL::VARCHAR AS semantics, \
            NULL::VARCHAR AS aggregation_rule, NULL::VARCHAR AS unit, NULL::VARCHAR AS description, NULL::BOOLEAN AS nullable LIMIT 0".into()));
    }
    let rows: Vec<String> = fields
        .iter()
        .map(|f| {
            let name = f.get("name").and_then(Json::as_str).unwrap_or("");
            format!(
                "({}, {}, {}, {}, {}, {}, {})",
                lit(name),
                slit(f.get("parquet_type")),
                slit(f.get("semantics")),
                slit(f.get("aggregation_rule")),
                slit(f.get("unit")),
                slit(info.pointer(&format!(
                    "/columns/{}/description",
                    name.replace('~', "~0").replace('/', "~1")
                ))),
                if f.get("nullable").and_then(Json::as_bool).unwrap_or(true) {
                    "true"
                } else {
                    "false"
                },
            )
        })
        .collect();
    Ok(Plan::Query(format!(
        "SELECT * FROM (VALUES {}) AS t(\"column\", parquet_type, semantics, aggregation_rule, unit, description, nullable)",
        rows.join(", ")
    )))
}
