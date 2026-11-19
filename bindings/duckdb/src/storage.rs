//! File access for writers: local paths and `s3://` (S3, MinIO, R2, ... via an endpoint).
//!
//! Parquet data is written by DuckDB `COPY`, which uses DuckDB's own S3 secret. This module
//! covers what DuckDB cannot do: write arbitrary bytes (Iceberg Avro/JSON metadata), read
//! them back, stat objects, and delete objects for `overwrite`.
//!
//! S3 settings, first match wins:
//! 1. `s3_*` named parameters of the SQL function;
//! 2. the DuckDB S3 secret that `which_secret(path, 's3')` picks: region, endpoint,
//!    url_style and use_ssl (DuckDB never reveals the secret key itself);
//! 3. AWS environment variables, then the shared `~/.aws/credentials` / `~/.aws/config`
//!    profile (`AWS_PROFILE`, default `default`);
//! 4. container / instance / web-identity credentials (object_store's chain).

use crate::sql::{self, lit, Lease, E};

/// Explicit S3 overrides from `s3_*` parameters.
#[derive(Clone, Default, Debug)]
pub struct S3Params {
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub url_style: Option<String>,
    pub use_ssl: Option<bool>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
}

pub fn is_s3(path: &str) -> bool {
    path.starts_with("s3://") || path.starts_with("s3a://")
}

pub fn is_local(path: &str) -> bool {
    !path.contains("://") || path.starts_with("file://")
}

pub fn local(path: &str) -> &str {
    path.strip_prefix("file://").unwrap_or(path)
}

/// Split `s3://bucket/key` into (bucket, key).
pub fn split_s3(path: &str) -> Result<(String, String), E> {
    let rest = path
        .strip_prefix("s3://")
        .or_else(|| path.strip_prefix("s3a://"))
        .ok_or_else(|| format!("not an s3:// path: {path}"))?;
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    if bucket.is_empty() {
        return Err(format!("no bucket in {path}").into());
    }
    Ok((bucket.to_string(), key.to_string()))
}

/// Resolved storage for one write operation.
pub struct Storage {
    #[cfg(feature = "native-s3")]
    s3: Option<s3::Conf>,
}

impl Storage {
    /// Resolve settings for `root`. Remote non-S3 schemes are refused for byte writes.
    pub fn for_path(lease: &Lease, root: &str, params: &S3Params) -> Result<Self, E> {
        if is_local(root) {
            return Ok(Storage {
                #[cfg(feature = "native-s3")]
                s3: None,
            });
        }
        if !is_s3(root) {
            return Err(format!(
                "writing metadata to {root} is not supported; use a local path or s3://"
            )
            .into());
        }
        #[cfg(feature = "native-s3")]
        {
            Ok(Storage {
                s3: Some(s3::Conf::resolve(lease, root, params)?),
            })
        }
        #[cfg(not(feature = "native-s3"))]
        {
            let _ = (lease, params);
            Err("this build has no native S3 client (it is left out of the Wasm build)".into())
        }
    }

    pub fn put(&self, path: &str, bytes: Vec<u8>) -> Result<(), E> {
        if is_local(path) {
            let p = std::path::Path::new(local(path));
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            return Ok(std::fs::write(p, bytes)?);
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.put(path, bytes);
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    pub fn get(&self, path: &str) -> Result<Vec<u8>, E> {
        if is_local(path) {
            return Ok(std::fs::read(local(path)).map_err(|e| format!("cannot read {path}: {e}"))?);
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.get(path);
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    pub fn size(&self, path: &str) -> Result<i64, E> {
        if is_local(path) {
            return Ok(std::fs::metadata(local(path))?.len() as i64);
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.size(path);
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    /// Upload local files to their remote paths, `concurrency` requests at a time.
    pub fn upload_files(
        &self,
        files: &[(String, std::path::PathBuf)],
        concurrency: usize,
    ) -> Result<(), E> {
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.upload_files(files, concurrency);
        }
        #[allow(unreachable_code)]
        {
            let _ = (files, concurrency);
            Err("no S3 client".into())
        }
    }

    /// Delete everything under `prefix` (a folder). Returns the number of objects removed.
    pub fn delete_prefix(&self, prefix: &str) -> Result<usize, E> {
        if is_local(prefix) {
            let p = std::path::Path::new(local(prefix));
            if p.is_dir() {
                let n = walk_count(p);
                std::fs::remove_dir_all(p)?;
                return Ok(n);
            }
            if p.is_file() {
                std::fs::remove_file(p)?;
                return Ok(1);
            }
            return Ok(0);
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.delete_prefix(prefix);
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    /// A tag of the object's current version (ETag on S3), None when it does not exist.
    pub fn version_tag(&self, path: &str) -> Result<Option<String>, E> {
        if is_local(path) {
            return Ok(std::fs::metadata(local(path)).ok().map(|m| {
                let t = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                format!("{}-{t}", m.len())
            }));
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.version_tag(path);
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    /// Write only if the object is still at version `expected` (None: it must not exist), so
    /// two writers committing at once cannot both succeed. On S3 this is a conditional PUT
    /// (If-Match / If-None-Match); locally a check right before the write.
    pub fn put_if(&self, path: &str, bytes: Vec<u8>, expected: Option<&str>) -> Result<(), E> {
        let conflict = || -> E { format!("{path} was changed by another writer; retry").into() };
        if is_local(path) {
            if self.version_tag(path)?.as_deref() != expected {
                return Err(conflict());
            }
            return self.put(path, bytes);
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.put_if(path, bytes, expected).map_err(|e| {
                if e.to_string().contains("precondition")
                    || e.to_string().contains("already exists")
                {
                    conflict()
                } else {
                    e
                }
            });
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    /// Every object below `prefix` (recursive): (path, size, last modified ms since epoch).
    pub fn list(&self, prefix: &str) -> Result<Vec<(String, u64, i64)>, E> {
        if is_local(prefix) {
            let mut out = Vec::new();
            walk_files(std::path::Path::new(local(prefix)), &mut out);
            let base = local(prefix).trim_end_matches('/');
            let shown = prefix.trim_end_matches('/');
            return Ok(out
                .into_iter()
                .map(|(p, n, t)| {
                    let rel = p.strip_prefix(base).unwrap_or(&p).to_string();
                    (format!("{shown}{rel}"), n, t)
                })
                .collect());
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.list(prefix);
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    /// Immediate sub-folders of `prefix` (full paths, no trailing slash).
    pub fn list_dirs(&self, prefix: &str) -> Result<Vec<String>, E> {
        if is_local(prefix) {
            let shown = prefix.trim_end_matches('/');
            let mut out: Vec<String> = std::fs::read_dir(local(prefix))
                .map_err(|e| format!("cannot list {prefix}: {e}"))?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| format!("{shown}/{}", e.file_name().to_string_lossy()))
                .collect();
            out.sort();
            return Ok(out);
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.list_dirs(prefix);
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    /// Read many objects with `threads` requests at a time, in the order given.
    pub fn get_many(&self, paths: &[String], threads: usize) -> Result<Vec<Vec<u8>>, E> {
        let threads = threads.max(1).min(paths.len().max(1));
        let chunk = paths.len().div_ceil(threads).max(1);
        let parts: Vec<Result<Vec<Vec<u8>>, String>> = std::thread::scope(|sc| {
            let handles: Vec<_> = paths
                .chunks(chunk)
                .map(|c| {
                    sc.spawn(move || {
                        c.iter()
                            .map(|p| self.get(p).map_err(|e| e.to_string()))
                            .collect::<Result<Vec<_>, String>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("reader thread panicked".into()))
                })
                .collect()
        });
        let mut out = Vec::with_capacity(paths.len());
        for p in parts {
            out.extend(p?);
        }
        Ok(out)
    }

    /// Delete these objects (files, not folders). Returns how many were deleted.
    pub fn delete_many(&self, paths: &[String]) -> Result<usize, E> {
        if paths.is_empty() {
            return Ok(0);
        }
        if paths.iter().all(|p| is_local(p)) {
            let mut n = 0;
            for p in paths {
                match std::fs::remove_file(local(p)) {
                    Ok(()) => n += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("cannot delete {p}: {e}").into()),
                }
            }
            return Ok(n);
        }
        #[cfg(feature = "native-s3")]
        {
            return self.conf()?.delete_many(paths);
        }
        #[allow(unreachable_code)]
        Err("no S3 client".into())
    }

    #[cfg(feature = "native-s3")]
    fn conf(&self) -> Result<&s3::Conf, E> {
        self.s3
            .as_ref()
            .ok_or_else(|| "S3 storage is not configured for this path".into())
    }
}

fn walk_files(p: &std::path::Path, out: &mut Vec<(String, u64, i64)>) {
    let Ok(it) = std::fs::read_dir(p) else { return };
    for e in it.filter_map(|e| e.ok()) {
        let path = e.path();
        if path.is_dir() {
            walk_files(&path, out);
        } else if let Ok(m) = e.metadata() {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            out.push((path.to_string_lossy().into_owned(), m.len(), t));
        }
    }
}

fn walk_count(p: &std::path::Path) -> usize {
    std::fs::read_dir(p)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .map(|e| {
                    if e.path().is_dir() {
                        walk_count(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Non-secret fields of the DuckDB secret DuckDB would use for `path`.
fn duckdb_secret_fields(lease: &Lease, path: &str) -> Vec<(String, String)> {
    let name = sql::query_text_on(
        lease,
        &format!("SELECT name FROM which_secret({}, 's3')", lit(path)),
    )
    .ok()
    .and_then(|r| r.into_iter().next())
    .and_then(|r| r.into_iter().next().flatten());
    let Some(name) = name else { return Vec::new() };
    let text = sql::query_text_on(
        lease,
        &format!(
            "SELECT secret_string FROM duckdb_secrets() WHERE name = {}",
            lit(&name)
        ),
    )
    .ok()
    .and_then(|r| r.into_iter().next())
    .and_then(|r| r.into_iter().next().flatten())
    .unwrap_or_default();
    text.split(';')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// `[section]` → key/value pairs of an AWS shared config/credentials file.
fn ini_section(text: &str, section: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') && l.ends_with(']') {
            inside = l[1..l.len() - 1].trim() == section;
        } else if inside {
            if let Some((k, v)) = l.split_once('=') {
                out.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

#[cfg(feature = "native-s3")]
mod s3 {
    use super::*;
    use futures::TryStreamExt;
    use object_store::{
        aws::{AmazonS3, AmazonS3Builder},
        path::Path as ObjPath,
        ObjectStore, ObjectStoreExt, PutPayload,
    };
    use std::sync::Arc;

    pub struct Conf {
        region: String,
        endpoint: Option<String>,
        allow_http: bool,
        virtual_hosted: bool,
        keys: Option<(String, String, Option<String>)>,
    }

    fn runtime() -> Result<tokio::runtime::Runtime, E> {
        Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?)
    }

    impl Conf {
        pub fn resolve(lease: &Lease, root: &str, p: &S3Params) -> Result<Self, E> {
            let secret = duckdb_secret_fields(lease, root);
            let from_secret = |k: &str| {
                secret
                    .iter()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.clone())
                    .filter(|v| !v.is_empty())
            };
            let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            let home = env("HOME")
                .or_else(|| env("USERPROFILE"))
                .unwrap_or_default();
            let profile = env("AWS_PROFILE").unwrap_or_else(|| "default".into());
            let creds_file = env("AWS_SHARED_CREDENTIALS_FILE")
                .unwrap_or_else(|| format!("{home}/.aws/credentials"));
            let config_file =
                env("AWS_CONFIG_FILE").unwrap_or_else(|| format!("{home}/.aws/config"));
            let creds = std::fs::read_to_string(&creds_file)
                .map(|t| ini_section(&t, &profile))
                .unwrap_or_default();
            let config_section = if profile == "default" {
                "default".to_string()
            } else {
                format!("profile {profile}")
            };
            let config = std::fs::read_to_string(&config_file)
                .map(|t| ini_section(&t, &config_section))
                .unwrap_or_default();
            let ini = |v: &Vec<(String, String)>, k: &str| {
                v.iter().find(|(n, _)| n == k).map(|(_, x)| x.clone())
            };

            let region = p
                .region
                .clone()
                .or_else(|| from_secret("region"))
                .or_else(|| env("AWS_REGION"))
                .or_else(|| env("AWS_DEFAULT_REGION"))
                .or_else(|| ini(&config, "region"))
                .unwrap_or_else(|| "us-east-1".into());
            let use_ssl = p
                .use_ssl
                .or_else(|| from_secret("use_ssl").map(|v| v != "false"))
                .unwrap_or(true);
            let endpoint = p
                .endpoint
                .clone()
                .or_else(|| from_secret("endpoint").filter(|e| e != "s3.amazonaws.com"))
                .or_else(|| env("AWS_ENDPOINT_URL_S3"))
                .or_else(|| env("AWS_ENDPOINT_URL"))
                .map(|e| {
                    if e.contains("://") {
                        e
                    } else {
                        format!("{}://{e}", if use_ssl { "https" } else { "http" })
                    }
                });
            let url_style = p
                .url_style
                .clone()
                .or_else(|| from_secret("url_style"))
                .unwrap_or_else(|| "vhost".into());
            let keys = match (&p.access_key_id, &p.secret_access_key) {
                (Some(k), Some(s)) => Some((k.clone(), s.clone(), p.session_token.clone())),
                _ => match (env("AWS_ACCESS_KEY_ID"), env("AWS_SECRET_ACCESS_KEY")) {
                    (Some(k), Some(s)) => Some((k, s, env("AWS_SESSION_TOKEN"))),
                    _ => match (
                        ini(&creds, "aws_access_key_id"),
                        ini(&creds, "aws_secret_access_key"),
                    ) {
                        (Some(k), Some(s)) => Some((k, s, ini(&creds, "aws_session_token"))),
                        _ => None, // container / instance / web identity
                    },
                },
            };
            Ok(Conf {
                region,
                allow_http: endpoint
                    .as_deref()
                    .is_some_and(|e| e.starts_with("http://")),
                virtual_hosted: url_style != "path",
                endpoint,
                keys,
            })
        }

        fn store(&self, bucket: &str) -> Result<Arc<AmazonS3>, E> {
            let mut b = AmazonS3Builder::from_env()
                .with_bucket_name(bucket)
                .with_region(&self.region);
            if let Some((k, s, t)) = &self.keys {
                b = b.with_access_key_id(k).with_secret_access_key(s);
                if let Some(t) = t {
                    b = b.with_token(t);
                }
            }
            if let Some(e) = &self.endpoint {
                b = b
                    .with_endpoint(e)
                    .with_allow_http(self.allow_http)
                    .with_virtual_hosted_style_request(self.virtual_hosted);
            }
            Ok(Arc::new(b.build()?))
        }

        pub fn put(&self, path: &str, bytes: Vec<u8>) -> Result<(), E> {
            let (bucket, key) = split_s3(path)?;
            let store = self.store(&bucket)?;
            runtime()?.block_on(async move {
                store
                    .put(&ObjPath::from(key), PutPayload::from(bytes))
                    .await
            })?;
            Ok(())
        }

        pub fn version_tag(&self, path: &str) -> Result<Option<String>, E> {
            let (bucket, key) = split_s3(path)?;
            let store = self.store(&bucket)?;
            match runtime()?.block_on(async move { store.head(&ObjPath::from(key)).await }) {
                Ok(m) => Ok(Some(m.e_tag.unwrap_or_default())),
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(e.into()),
            }
        }

        pub fn put_if(&self, path: &str, bytes: Vec<u8>, expected: Option<&str>) -> Result<(), E> {
            use object_store::{PutMode, PutOptions, UpdateVersion};
            let (bucket, key) = split_s3(path)?;
            let store = self.store(&bucket)?;
            let mode = match expected {
                Some(tag) => PutMode::Update(UpdateVersion {
                    e_tag: Some(tag.to_string()),
                    version: None,
                }),
                None => PutMode::Create,
            };
            let opts = PutOptions {
                mode,
                ..Default::default()
            };
            runtime()?.block_on(async move {
                store
                    .put_opts(&ObjPath::from(key), PutPayload::from(bytes), opts)
                    .await
            })?;
            Ok(())
        }

        pub fn get(&self, path: &str) -> Result<Vec<u8>, E> {
            let (bucket, key) = split_s3(path)?;
            let store = self.store(&bucket)?;
            let bytes = runtime()?
                .block_on(async move { store.get(&ObjPath::from(key)).await?.bytes().await })?;
            Ok(bytes.to_vec())
        }

        pub fn size(&self, path: &str) -> Result<i64, E> {
            let (bucket, key) = split_s3(path)?;
            let store = self.store(&bucket)?;
            let meta = runtime()?.block_on(async move { store.head(&ObjPath::from(key)).await })?;
            Ok(meta.size as i64)
        }

        pub fn upload_files(
            &self,
            files: &[(String, std::path::PathBuf)],
            concurrency: usize,
        ) -> Result<(), E> {
            use futures::StreamExt;
            let mut stores: std::collections::HashMap<String, Arc<AmazonS3>> = Default::default();
            let mut jobs = Vec::with_capacity(files.len());
            for (remote, src) in files {
                let (bucket, key) = split_s3(remote)?;
                if !stores.contains_key(&bucket) {
                    stores.insert(bucket.clone(), self.store(&bucket)?);
                }
                jobs.push((stores[&bucket].clone(), key, src.clone()));
            }
            runtime()?.block_on(async move {
                futures::stream::iter(jobs.into_iter().map(|(store, key, src)| async move {
                    let bytes = std::fs::read(&src)
                        .map_err(|e| format!("cannot read staged file {}: {e}", src.display()))?;
                    store
                        .put(&ObjPath::from(key.clone()), PutPayload::from(bytes))
                        .await
                        .map_err(|e| format!("upload of {key} failed: {e}"))?;
                    Ok::<(), String>(())
                }))
                .buffer_unordered(concurrency.max(1))
                .try_collect::<Vec<()>>()
                .await
            })?;
            Ok(())
        }

        pub fn list(&self, prefix: &str) -> Result<Vec<(String, u64, i64)>, E> {
            let (bucket, key) = split_s3(prefix)?;
            let store = self.store(&bucket)?;
            let base = ObjPath::from(key.trim_end_matches('/').to_string());
            let items: Vec<object_store::ObjectMeta> =
                runtime()?.block_on(async move { store.list(Some(&base)).try_collect().await })?;
            Ok(items
                .into_iter()
                .map(|o| {
                    (
                        format!("s3://{bucket}/{}", o.location),
                        o.size,
                        o.last_modified.timestamp_millis(),
                    )
                })
                .collect())
        }

        pub fn list_dirs(&self, prefix: &str) -> Result<Vec<String>, E> {
            let (bucket, key) = split_s3(prefix)?;
            let store = self.store(&bucket)?;
            let base = ObjPath::from(key.trim_end_matches('/').to_string());
            let r =
                runtime()?.block_on(async move { store.list_with_delimiter(Some(&base)).await })?;
            let mut out: Vec<String> = r
                .common_prefixes
                .into_iter()
                .map(|p| format!("s3://{bucket}/{p}"))
                .collect();
            out.sort();
            Ok(out)
        }

        pub fn delete_many(&self, paths: &[String]) -> Result<usize, E> {
            use futures::StreamExt;
            let mut by_bucket: std::collections::HashMap<String, Vec<ObjPath>> = Default::default();
            for p in paths {
                let (bucket, key) = split_s3(p)?;
                by_bucket
                    .entry(bucket)
                    .or_default()
                    .push(ObjPath::from(key));
            }
            let mut n = 0;
            for (bucket, keys) in by_bucket {
                let store = self.store(&bucket)?;
                n += runtime()?.block_on(async move {
                    let stream = futures::stream::iter(keys.into_iter().map(Ok)).boxed();
                    let done: Vec<ObjPath> = store.delete_stream(stream).try_collect().await?;
                    Ok::<usize, object_store::Error>(done.len())
                })?;
            }
            Ok(n)
        }

        pub fn delete_prefix(&self, prefix: &str) -> Result<usize, E> {
            let (bucket, key) = split_s3(prefix)?;
            let store = self.store(&bucket)?;
            runtime()?
                .block_on(async move {
                    let key = key.trim_end_matches('/').to_string();
                    let base = ObjPath::from(key.clone());
                    let mut n = 0;
                    // The object itself (a file), then everything below it (a folder).
                    if store.head(&base).await.is_ok() {
                        store.delete(&base).await?;
                        n += 1;
                    }
                    let items: Vec<_> = store.list(Some(&base)).try_collect().await?;
                    for o in items {
                        store.delete(&o.location).await?;
                        n += 1;
                    }
                    Ok::<usize, object_store::Error>(n)
                })
                .map_err(|e| e.into())
        }
    }
}
