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
    /// Iceberg snapshot to read instead of the current one (snapshot_id / as_of).
    pub snapshot: Option<i64>,
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
            snapshot: None,
        });
    }
    // An Iceberg table with a version hint: two small reads, no listing.
    if let Some((metadata, j)) = hinted_metadata(lease, &path) {
        let manifest = iceberg_manifest(&j)?;
        return Ok(Source {
            root: path,
            layout: Layout::Iceberg { metadata },
            manifest,
            snapshot: None,
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
            snapshot: None,
        });
    }
    if is_http(&path) || !glob(lease, &join(&path, "metadata/*.metadata.json"))?.is_empty() {
        let (metadata, j) = latest_iceberg_metadata(lease, &path)?;
        let manifest = iceberg_manifest(&j)?;
        return Ok(Source {
            root: path,
            layout: Layout::Iceberg { metadata },
            manifest,
            snapshot: None,
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
                    "iceberg_scan({}, version = {}, allow_moved_paths = true{})",
                    lit(dir),
                    lit(file.strip_suffix(".metadata.json").unwrap_or(file)),
                    src.snapshot
                        .map_or(String::new(), |id| format!(", snapshot_from_id = {id}"))
                ),
                None => format!(
                    "iceberg_scan({}{})",
                    lit(metadata),
                    src.snapshot
                        .map_or(String::new(), |id| format!(", snapshot_from_id = {id}"))
                ),
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
    let expr = format!(
        "cell_to_geometry({}, gid, {}, {})",
        lit(code),
        lit(&crs),
        lit(&format)
    );
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

/// Metadata as if snapshot `id` were the current one (its schema too).
fn meta_at_snapshot(meta: &Json, id: i64) -> Json {
    let mut m = meta.clone();
    let schema = m
        .get("snapshots")
        .and_then(Json::as_array)
        .and_then(|a| {
            a.iter()
                .find(|s| s.get("snapshot-id").and_then(Json::as_i64) == Some(id))
        })
        .and_then(|s| s.get("schema-id").cloned());
    m["current-snapshot-id"] = Json::from(id);
    if let Some(sid) = schema {
        m["current-schema-id"] = sid;
    }
    m
}

/// `snapshot_id := id` or `as_of := timestamp | epoch ms`: read an older Iceberg snapshot
/// (the newest one at or before `as_of`). Expired snapshots cannot be read.
pub fn select_snapshot(lease: &Lease, src: &mut Source, p: &Params) -> Result<(), E> {
    let id = p.int("snapshot_id");
    let as_of = p.text("as_of");
    if id.is_none() && as_of.is_none() {
        return Ok(());
    }
    let Layout::Iceberg { metadata } = &src.layout else {
        return Err("snapshot_id / as_of read older versions of Iceberg tables only".into());
    };
    let meta = metadata_json(lease, metadata)?;
    let snaps: Vec<(i64, i64)> = meta
        .get("snapshots")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some((
                s.get("snapshot-id")?.as_i64()?,
                s.get("timestamp-ms").and_then(Json::as_i64).unwrap_or(0),
            ))
        })
        .collect();
    let chosen = match (id, as_of) {
        (Some(id), _) => snaps
            .iter()
            .find(|(s, _)| *s == id)
            .map(|(s, _)| *s)
            .ok_or_else(|| format!("snapshot {id} is not in the table (expired or wrong id)"))?,
        (None, Some(text)) => {
            let ts = match text.trim().parse::<i64>() {
                Ok(ms) => ms,
                Err(_) => sql::query_text_on(
                    lease,
                    &format!("SELECT epoch_ms(CAST({} AS TIMESTAMPTZ))", lit(&text)),
                )?
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next().flatten())
                .and_then(|v| v.parse().ok())
                .ok_or("as_of must be a timestamp or epoch milliseconds")?,
            };
            snaps
                .iter()
                .filter(|(_, t)| *t <= ts)
                .max_by_key(|(_, t)| *t)
                .map(|(s, _)| *s)
                .ok_or_else(|| format!("the table has no snapshot at or before {text}"))?
        }
        _ => unreachable!(),
    };
    src.snapshot = Some(chosen);
    Ok(())
}

pub fn plan_read(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let lease = sql::lease()?;
    let mut src = open(&lease, &path)?;
    select_snapshot(&lease, &mut src, p)?;
    let id = if src.manifest.is_some() || p.text("domain_code").is_some() {
        Some(identity(&src, p)?) // also validates the manifest against the engine
    } else {
        None
    };
    let plan = Plan::Query(format!(
        "SELECT * FROM {}",
        from_clause(&lease, &src, None)?
    ));
    let plan = match id {
        Some((d, level)) => add_rollup(
            plan,
            p,
            &QueryCtx {
                code: d.code,
                level,
                manifest: src.manifest.clone(),
                weight: None,
                extra: None,
                present: None,
            },
        )?,
        None if p.int("rollup_level").is_some() => {
            return Err(
                "rollup_level needs the domain and level: the dataset has no cell manifest".into(),
            )
        }
        None => plan,
    };
    add_geometry(plan, p, id.map(|(d, _)| d.code))
}

/// What a query plan knows about its dataset, for the roll-up and the geometry.
pub struct QueryCtx {
    pub code: &'static str,
    pub level: u8,
    pub manifest: Option<Json>,
    /// Column with the share of each cell inside the query area (`coverage_ratio` or
    /// `length_ratio`); None when every cell counts fully (read_cell_dataset) or for points.
    pub weight: Option<&'static str>,
    /// The column the query adds per cell (`coverage_ratio`, `length_ratio`, `point_count`).
    pub extra: Option<&'static str>,
    /// Columns the plan returns, when known (an older snapshot may lack newer columns).
    pub present: Option<Vec<String>>,
}

/// SQL aggregate of value column `c` by rule, each cell weighted by `w`.
pub fn rollup_expr(name: &str, rule: &str, w: &str) -> Result<String, E> {
    let c = format!("r.{}", sql::ident(name));
    Ok(match rule.to_ascii_lowercase().as_str() {
        "sum" | "count" => format!("CAST(sum({c} * {w}) AS DOUBLE)"),
        "mean" | "avg" | "average" | "weighted_mean" => format!(
            "CAST(sum({c} * {w}) / nullif(sum(CASE WHEN {c} IS NOT NULL THEN {w} END), 0) AS DOUBLE)"
        ),
        "min" => format!("min({c})"),
        "max" => format!("max({c})"),
        other => {
            return Err(format!(
                "column {name} has aggregation_rule {other:?}, which rollup_level cannot apply; \
                 pass rollup_rule := '{{\"{name}\": \"sum\" | \"mean\" | \"min\" | \"max\"}}'"
            )
            .into())
        }
    })
}

/// `rollup_level := R`: one row per level-R ancestor cell instead of one per dataset cell.
/// Each value column is aggregated by its `aggregation_rule` from the cell manifest (sum:
/// weighted sum, mean: weighted mean, min, max; without a rule COUNT columns are summed and
/// the others averaged), weighted by the share of each cell inside the query area.
/// `rollup_rule` overrides the rule: one word for every column or `{column or pattern: rule}`.
/// Adds `cell_count` (dataset cells in the group) and, for areas, the ancestor's
/// `coverage_ratio` (its share inside the area).
fn add_rollup(plan: Plan, p: &Params, ctx: &QueryCtx) -> Result<Plan, E> {
    let Some(r) = p.int("rollup_level") else {
        if p.text("rollup_rule").is_some() {
            return Err("rollup_rule needs rollup_level".into());
        }
        return Ok(plan);
    };
    if r < 0 || r > ctx.level as i64 {
        return Err(format!(
            "rollup_level must be between 0 and the dataset level {}; received {r}",
            ctx.level
        )
        .into());
    }
    if ctx.weight.is_none() && p.text("geometry").is_some() && p.get("bbox").is_none() {
        if let Some(g) = p.text("geometry") {
            if matches!(geom::parse_geometry(&g)?, geom::Geometry::Points(_)) {
                return Err(
                    "rollup_level works with a polygon, a line or bbox, not with points".into(),
                );
            }
        }
    }
    let fields = ctx
        .manifest
        .as_ref()
        .and_then(|m| m.get("value_fields"))
        .and_then(Json::as_array)
        .filter(|f| !f.is_empty())
        .ok_or("rollup_level needs the value columns of the cell manifest (value_fields)")?;
    let over = crate::metadata::PerColumn::parse(p.text("rollup_rule"), "rollup_rule")?;
    let w = ctx
        .weight
        .map(|w| format!("r.{w}"))
        .unwrap_or_else(|| "1.0".into());
    let mut aggs = Vec::new();
    for f in fields {
        let name = f.get("name").and_then(Json::as_str).unwrap_or("");
        if name.is_empty()
            || ctx
                .present
                .as_ref()
                .is_some_and(|p| !p.iter().any(|c| c == name))
        {
            continue;
        }
        let rule = over
            .get(name)
            .map(str::to_string)
            .or_else(|| {
                f.get("aggregation_rule")
                    .and_then(Json::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| {
                if f.get("semantics").and_then(Json::as_str) == Some("COUNT") {
                    "sum".into()
                } else {
                    "mean".into()
                }
            });
        aggs.push(format!(
            "{} AS {}",
            rollup_expr(name, &rule, &w)?,
            sql::ident(name)
        ));
    }
    let r = r as u8;
    let side = |l: u8| codec::side_cell_count(l).map(|n| n as f64);
    let children = (side(ctx.level)? / side(r)?).powi(2);
    let coverage = match ctx.weight {
        Some(w) => format!(", CAST(sum(r.{w}) / {children:?} AS DOUBLE) AS {w}"),
        None => String::new(),
    };
    let wrap = |inner: &str| {
        format!(
            "SELECT left(r.gid, {r}) AS gid, pack({}, left(r.gid, {r})) AS packed_id, {}, \
             count(*) AS cell_count{coverage} FROM ({inner}) AS r GROUP BY 1 ORDER BY 2",
            lit(ctx.code),
            aggs.join(", ")
        )
    };
    Ok(match plan {
        Plan::Query(q) => Plan::Query(wrap(&q)),
        Plan::Typed(q, probe) => Plan::Typed(wrap(&q), wrap(&probe)),
        other => other,
    })
}

/// Value columns of a dataset (from its cell manifest).
fn value_columns(ctx: &QueryCtx) -> Vec<String> {
    ctx.manifest
        .as_ref()
        .and_then(|m| m.get("value_fields"))
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.get("name").and_then(Json::as_str).map(str::to_string))
        .filter(|n| ctx.present.as_ref().is_none_or(|p| p.contains(n)))
        .collect()
}

/// Short name of a dataset path for prefixing clashing columns: the last folder, without
/// `data_name=`.
fn dataset_alias(path: &str) -> String {
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path);
    last.strip_prefix("data_name=").unwrap_or(last).to_string()
}

/// query_cell_datasets([path, ...], <the parameters of query_cell_dataset>, fill_nulls := true)
///
/// Several datasets over one area in one call: one row per cell (or per `rollup_level` cell)
/// with the value columns of every dataset. A column name used by more than one dataset is
/// prefixed with the dataset's folder name (`demographyhires_v2023__population`). Cells that a
/// dataset lacks get 0 in its columns (`fill_nulls := false` keeps NULL). Without
/// `rollup_level` the datasets must share domain and level.
pub fn plan_query_multi(p: &Params) -> Result<Plan, E> {
    let paths: Vec<String> = match p.positional.first() {
        Some(Param::List(items)) => items
            .iter()
            .filter_map(|i| i.text().map(str::to_string))
            .collect(),
        Some(Param::Text(t)) => vec![t.clone()],
        _ => Vec::new(),
    };
    if paths.is_empty() {
        return Err("query_cell_datasets needs a list of dataset paths".into());
    }
    let rollup = p.int("rollup_level").is_some();
    let mut parts = Vec::new();
    for path in &paths {
        let (plan, ctx) = plan_query_cells(p, path)?;
        let plan = add_rollup(plan, p, &ctx)?;
        parts.push((path.clone(), plan, ctx));
    }
    let (code, level) = (parts[0].2.code, parts[0].2.level);
    for (path, _, ctx) in &parts {
        if ctx.code != code {
            return Err(format!(
                "{path} is in domain {}, the first dataset in {code}",
                ctx.code
            )
            .into());
        }
        if !rollup && ctx.level != level {
            return Err(format!(
                "{path} is at level {}, the first dataset at level {level}: pass rollup_level := <level> to join them",
                ctx.level
            )
            .into());
        }
    }
    // Columns per dataset; clashing names are prefixed.
    let cols: Vec<Vec<String>> = parts.iter().map(|(_, _, c)| value_columns(c)).collect();
    let mut count = std::collections::HashMap::<&str, usize>::new();
    for c in cols.iter().flatten() {
        *count.entry(c.as_str()).or_default() += 1;
    }
    let fill = p.get("fill_nulls").and_then(Param::int).unwrap_or(1) != 0;
    let key = if rollup { "gid" } else { "packed_id" };
    let n = parts.len();
    let coalesce = |c: &str| {
        let items: Vec<String> = (0..n).map(|i| format!("t{i}.{c}")).collect();
        if n == 1 {
            items[0].clone()
        } else {
            format!("coalesce({})", items.join(", "))
        }
    };
    let mut select = vec![
        format!("{} AS packed_id", coalesce("packed_id")),
        format!("{} AS gid", coalesce("gid")),
    ];
    let names: Vec<String> = parts.iter().map(|(p, _, _)| dataset_alias(p)).collect();
    let aliases: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(i, a)| {
            if names.iter().filter(|b| *b == a).count() > 1 {
                format!("{a}_{}", i + 1)
            } else {
                a.clone()
            }
        })
        .collect();
    for (i, _) in parts.iter().enumerate() {
        let alias = &aliases[i];
        for c in &cols[i] {
            let out = if count[c.as_str()] > 1 {
                format!("{alias}__{c}")
            } else {
                c.clone()
            };
            let v = format!("t{i}.{}", sql::ident(c));
            let v = if fill { format!("coalesce({v}, 0)") } else { v };
            select.push(format!("{v} AS {}", sql::ident(&out)));
        }
    }
    if rollup {
        let items: Vec<String> = (0..n).map(|i| format!("t{i}.cell_count")).collect();
        select.push(format!("list_max([{}]) AS cell_count", items.join(", ")));
    }
    if let Some(extra) = parts[0].2.extra {
        select.push(format!("{} AS {extra}", coalesce(extra)));
    }
    let join = |sqls: &[String]| {
        let mut from = format!("({}) AS t0", sqls[0]);
        for (i, s) in sqls.iter().enumerate().skip(1) {
            let prev: Vec<String> = (0..i).map(|k| format!("t{k}.{key}")).collect();
            let on = if i == 1 {
                prev[0].clone()
            } else {
                format!("coalesce({})", prev.join(", "))
            };
            from.push_str(&format!(" FULL JOIN ({s}) AS t{i} ON t{i}.{key} = {on}"));
        }
        format!("SELECT {} FROM {from} ORDER BY 1", select.join(", "))
    };
    let typed = parts.iter().all(|(_, pl, _)| matches!(pl, Plan::Typed(..)));
    let plan = if typed {
        let (q, pr): (Vec<String>, Vec<String>) = parts
            .iter()
            .map(|(_, pl, _)| match pl {
                Plan::Typed(q, pr) => (q.clone(), pr.clone()),
                _ => unreachable!(),
            })
            .unzip();
        Plan::Typed(join(&q), join(&pr))
    } else {
        let q: Vec<String> = parts
            .iter()
            .map(|(_, pl, _)| match pl {
                Plan::Query(q) | Plan::Typed(q, _) => Ok(q.clone()),
                _ => Err::<String, E>("unexpected plan".into()),
            })
            .collect::<Result<_, _>>()?;
        Plan::Query(join(&q))
    };
    add_geometry(plan, p, Some(code))
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
    let (plan, ctx) = plan_query_cells(p, &path)?;
    let plan = add_rollup(plan, p, &ctx)?;
    add_geometry(plan, p, Some(ctx.code))
}

/// The cells of the query area joined with the dataset, and the dataset's domain code.
fn plan_query_cells(p: &Params, path: &str) -> Result<(Plan, QueryCtx), E> {
    let path = path.to_string();
    let lease = sql::lease()?;
    let mut src = open(&lease, &path)?;
    select_snapshot(&lease, &mut src, p)?;
    let (d, level) = identity(&src, p)?;
    let limit = p.int("max_candidate_limit").unwrap_or(100_000);
    let (wkt, crs, min_cov) = query_polygon(p, d, level)?;
    let shape = geom::parse_geometry(&wkt)?;

    // Plan in Rust: the cells decide which partitions and which packed_id range to read,
    // and `keys` is the matching cell table for the join inside DuckDB.
    let (cells, keys, extra, extra_type): (Vec<Cell>, String, &str, &str) = match shape {
        geom::Geometry::Polygons(polys) => {
            let opts = polyfill::PolyfillOptions {
                source_crs: crs.clone(),
                coverage_mode: p
                    .text("coverage_mode")
                    .unwrap_or_else(|| "GRID_PLANAR".into()),
                min_coverage: min_cov,
                max_candidate_limit: limit,
            };
            let cells = polyfill::polyfill(d, polys, level, &opts)?;
            let keys = format!(
                "polygon_to_cells({}, {}, {}, source_crs := {}, coverage_mode := {}, min_coverage := {}, max_candidate_limit := {})",
                lit(d.code), lit(&wkt), level, lit(&crs), lit(&opts.coverage_mode), opts.min_coverage, limit
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
    let mut ctx = QueryCtx {
        code: d.code,
        level,
        manifest: src.manifest.clone(),
        weight: (extra != "point_count").then_some(extra),
        extra: Some(extra),
        present: None,
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
        let (key, meta) = match src.snapshot {
            Some(id) => (
                format!("{metadata}#{id}"),
                meta_at_snapshot(&metadata_json(&lease, metadata)?, id),
            ),
            None => (metadata.clone(), metadata_json(&lease, metadata)?),
        };
        if let Some(t) = crate::icefast::load(&lease, &src.root, &key, &meta)? {
            let mut sorted = packed.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if let Some(sel) = t.files_for(&lease, &sorted)? {
                if sel.is_empty() {
                    ctx.present = Some(t.columns.iter().map(|(n, _)| n.clone()).collect());
                    return Ok((
                        Plan::Query(format!(
                            "SELECT {}, NULL::{extra_type} AS {extra} LIMIT 0",
                            t.empty_select()
                        )),
                        ctx,
                    ));
                }
                ctx.present = Some(t.columns.iter().map(|(n, _)| n.clone()).collect());
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
                ), ctx));
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
                ctx,
            ));
        }
        Some(f) => from_clause(&lease, &src, Some(f))?,
        None => full,
    };
    let cols = columns(&lease, &from)?;
    ctx.present = Some(cols.clone());
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
    Ok((Plan::Query(sql), ctx))
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
    let info = dataset_info(&lease, &src)?;
    Ok((src, manifest, info))
}

/// The dataset_info document (title, description, period, version, column descriptions).
pub fn dataset_info(lease: &Lease, src: &Source) -> Result<Json, E> {
    Ok(match &src.layout {
        Layout::Iceberg { metadata } => metadata_json(lease, metadata)?
            .pointer(&format!(
                "/properties/{}",
                crate::metadata::INFO_KEY.replace('/', "~1")
            ))
            .and_then(Json::as_str)
            .and_then(|t| serde_json::from_str(t).ok()),
        _ => read_text(lease, &join(&src.root, crate::metadata::INFO_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok()),
    }
    .unwrap_or(Json::Null))
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
