//! Upkeep of catalog-less Iceberg cell datasets.
//!
//! `expire_snapshots(path, retain_last := 1, older_than := NULL, orphan_age_hours := 24,
//! dry_run := false)`: drop old snapshots from the table metadata and delete the files only
//! they used (data files, manifests, manifest lists, older metadata files). Files in the table
//! folders that no metadata refers to (left by a failed or running write) are deleted only when
//! older than `orphan_age_hours`, so a write in progress is never touched.

use crate::{
    avro::{self, D},
    dataset::{self, join},
    sql::{self, lit, Lease, Params, Plan, E},
    storage::Storage,
    writer,
};
use serde_json::{json, Value as Json};
use std::collections::HashSet;

/// Paths in metadata are absolute under the table `location`; the table may be read from
/// another folder (moved / copied), so map them to `root`.
fn relocate(path: &str, location: &str, root: &str) -> String {
    let loc = location.trim_end_matches('/');
    match path.strip_prefix(loc) {
        Some(rest) => format!("{}{}", root.trim_end_matches('/'), rest),
        None => path.to_string(),
    }
}

/// Manifest paths of a manifest list, and the live data files of each manifest.
fn snapshot_files(
    store: &Storage,
    list: &str,
    loc: &str,
    root: &str,
    manifests: &mut HashSet<String>,
    data: &mut HashSet<String>,
    seen: &mut HashSet<String>,
) -> Result<(), E> {
    let entries = crate::iceberg::read_manifest_list(&store.get(list)?)?;
    for m in entries {
        let path = relocate(&m.path, loc, root);
        manifests.insert(path.clone());
        if !seen.insert(path.clone()) {
            continue; // shared by several snapshots: read once
        }
        let (_, records) = avro::read_container(&store.get(&path)?)?;
        for e in records {
            // status 2 = DELETED in this snapshot: not a live file of it
            if e.get("status").and_then(D::long) == Some(2) {
                continue;
            }
            if let Some(D::Str(f)) = e.get("data_file").and_then(|d| d.get("file_path")) {
                data.insert(relocate(f, loc, root));
            }
        }
    }
    Ok(())
}

fn ts_param(p: &Params, name: &str) -> Result<Option<i64>, E> {
    let Some(text) = p.text(name) else {
        return Ok(None);
    };
    if let Ok(ms) = text.trim().parse::<i64>() {
        return Ok(Some(ms));
    }
    // A timestamp text: let DuckDB parse it.
    let lease = sql::lease()?;
    let r = sql::query_text_on(
        &lease,
        &format!("SELECT epoch_ms(CAST({} AS TIMESTAMPTZ))", lit(&text)),
    )?;
    r.into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten())
        .and_then(|v| v.parse().ok())
        .map(Some)
        .ok_or_else(|| format!("{name} must be a timestamp or epoch milliseconds").into())
}

pub fn plan_expire(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let retain = p.int("retain_last").unwrap_or(1);
    if retain < 1 {
        return Err("retain_last must be at least 1".into());
    }
    let older_than = ts_param(p, "older_than")?;
    let orphan_hours = p.float("orphan_age_hours").unwrap_or(24.0);
    if orphan_hours < 0.0 {
        return Err("orphan_age_hours must not be negative".into());
    }
    let dry = p.get("dry_run").and_then(sql::Param::int).unwrap_or(0) != 0;
    let s3 = writer::s3_params_pub(p);
    Ok(Plan::Action(
        Box::new(move || expire(&path, retain as usize, older_than, orphan_hours, dry, &s3)),
        [
            "snapshots_kept",
            "snapshots_expired",
            "files_deleted",
            "bytes_deleted",
            "orphans_kept",
            "metadata",
            "dry_run",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    ))
}

fn expire(
    path: &str,
    retain: usize,
    older_than: Option<i64>,
    orphan_hours: f64,
    dry: bool,
    s3: &crate::storage::S3Params,
) -> Result<Vec<Vec<Option<String>>>, E> {
    let lease: Lease = sql::lease()?;
    let root = path.trim_end_matches('/').to_string();
    let store = Storage::for_path(&lease, &root, s3)?;
    let src = dataset::open(&lease, &root)?;
    let dataset::Layout::Iceberg { metadata } = &src.layout else {
        return Err("expire_snapshots is for Iceberg tables".into());
    };
    let hint_path = join(&root, "metadata/version-hint.text");
    let hint_tag = store.version_tag(&hint_path)?;
    let meta = dataset::metadata_json(&lease, metadata)?;
    let loc = meta
        .get("location")
        .and_then(Json::as_str)
        .unwrap_or(&root)
        .to_string();
    let current = meta.get("current-snapshot-id").and_then(Json::as_i64);
    let mut snaps: Vec<Json> = meta
        .get("snapshots")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    snaps.sort_by_key(|s| {
        std::cmp::Reverse(s.get("timestamp-ms").and_then(Json::as_i64).unwrap_or(0))
    });
    let mut keep_ids = HashSet::new();
    for (i, s) in snaps.iter().enumerate() {
        let id = s.get("snapshot-id").and_then(Json::as_i64);
        let ts = s.get("timestamp-ms").and_then(Json::as_i64).unwrap_or(0);
        if id == current || i < retain || older_than.is_some_and(|t| ts >= t) {
            keep_ids.insert(id);
        }
    }
    let (kept, expired): (Vec<Json>, Vec<Json>) = snaps
        .into_iter()
        .partition(|s| keep_ids.contains(&s.get("snapshot-id").and_then(Json::as_i64)));

    // Files every kept snapshot needs.
    let (mut lists, mut manifests, mut data, mut seen) = (
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    );
    for s in &kept {
        if let Some(l) = s.get("manifest-list").and_then(Json::as_str) {
            let l = relocate(l, &loc, &root);
            lists.insert(l.clone());
            snapshot_files(
                &store,
                &l,
                &loc,
                &root,
                &mut manifests,
                &mut data,
                &mut seen,
            )?;
        }
    }
    // Files the expired snapshots used (deleted whatever their age).
    let (mut old_lists, mut old_manifests, mut old_data) =
        (HashSet::new(), HashSet::new(), HashSet::new());
    for s in &expired {
        if let Some(l) = s.get("manifest-list").and_then(Json::as_str) {
            let l = relocate(l, &loc, &root);
            if store.get(&l).is_err() {
                continue; // already gone
            }
            old_lists.insert(l.clone());
            snapshot_files(
                &store,
                &l,
                &loc,
                &root,
                &mut old_manifests,
                &mut old_data,
                &mut seen,
            )?;
        }
    }

    // The new metadata: kept snapshots only, history restarted.
    let mut next = meta.clone();
    let obj = next.as_object_mut().ok_or("metadata is not an object")?;
    let kept_ids: HashSet<i64> = kept
        .iter()
        .filter_map(|s| s.get("snapshot-id").and_then(Json::as_i64))
        .collect();
    let mut kept_sorted = kept.clone();
    kept_sorted.sort_by_key(|s| s.get("timestamp-ms").and_then(Json::as_i64).unwrap_or(0));
    obj.insert("snapshots".into(), Json::Array(kept_sorted));
    let log: Vec<Json> = obj
        .get("snapshot-log")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| {
            e.get("snapshot-id")
                .and_then(Json::as_i64)
                .is_some_and(|i| kept_ids.contains(&i))
        })
        .collect();
    obj.insert("snapshot-log".into(), Json::Array(log));
    obj.insert("metadata-log".into(), json!([]));
    if let Some(refs) = obj.get_mut("refs").and_then(Json::as_object_mut) {
        refs.retain(|_, r| {
            r.get("snapshot-id")
                .and_then(Json::as_i64)
                .is_some_and(|i| kept_ids.contains(&i))
        });
    }
    obj.insert("last-updated-ms".into(), json!(writer::now_ms()));

    let version = writer::max_metadata_version(&lease, &root)?.map_or(0, |v| v + 1);
    let stem = format!("{version:05}-{}", writer::uuid4());
    let new_meta = join(&root, &format!("metadata/{stem}.metadata.json"));

    // What is in the folders now.
    let now = writer::now_ms();
    let min_age_ms = (orphan_hours * 3_600_000.0) as i64;
    let mut files = store.list(&join(&root, "metadata"))?;
    files.extend(store.list(&join(&root, "data"))?);
    let live: HashSet<&String> = lists.iter().chain(&manifests).chain(&data).collect();
    // Safety: every file the kept snapshots need must be in the listing, else the paths in the
    // metadata and the folder do not match and nothing may be deleted.
    let listed: HashSet<&String> = files.iter().map(|(f, _, _)| f).collect();
    if let Some(missing) = live.iter().find(|f| !listed.contains(**f)) {
        return Err(format!(
            "{missing} is used by the table but not found under {root}; nothing was deleted"
        )
        .into());
    }
    let known_old: HashSet<&String> = old_lists
        .iter()
        .chain(&old_manifests)
        .chain(&old_data)
        .collect();
    let (mut delete, mut bytes, mut orphans_kept) = (Vec::new(), 0u64, 0usize);
    for (f, size, modified) in &files {
        if live.contains(f) || f.ends_with("/version-hint.text") {
            continue;
        }
        let old_metadata = f.ends_with(".metadata.json");
        if known_old.contains(f) || old_metadata || now - modified >= min_age_ms {
            delete.push(f.clone());
            bytes += size;
        } else {
            orphans_kept += 1;
        }
    }
    let expired_n = expired.len();
    if !dry && (expired_n > 0 || !delete.is_empty()) {
        if &dataset::latest_iceberg_metadata(&lease, &root)?.0 != metadata {
            return Err(format!("{root} changed during expire_snapshots; retry").into());
        }
        store.put(&new_meta, serde_json::to_vec_pretty(&next)?)?;
        store.put_if(&hint_path, stem.into_bytes(), hint_tag.as_deref())?;
        // Data and manifests first, metadata files last.
        let (meta_files, rest): (Vec<String>, Vec<String>) = delete
            .iter()
            .cloned()
            .partition(|f| f.ends_with(".metadata.json"));
        store.delete_many(&rest)?;
        store.delete_many(&meta_files)?;
    }
    Ok(vec![vec![
        Some(kept.len().to_string()),
        Some(expired_n.to_string()),
        Some(delete.len().to_string()),
        Some(bytes.to_string()),
        Some(orphans_kept.to_string()),
        Some(if dry || (expired_n == 0 && delete.is_empty()) {
            metadata.clone()
        } else {
            new_meta
        }),
        Some(dry.to_string()),
    ]])
}
