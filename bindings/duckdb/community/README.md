# Publishing geosquare_v2

## 1. Own extension repository (works today)

`.github/workflows/duckdb-release.yml` builds Linux (x86_64, arm64), macOS (arm64, x86_64),
Windows (x86_64) and DuckDB-Wasm (`wasm_eh`), tests every DuckDB release from 1.4.0, and
lays out a repository with `scripts/make_repository.py`:

```text
<repo>/v1.4.0/linux_amd64/geosquare_v2.duckdb_extension.gz
...
<repo>/v2.0.0/wasm_eh/geosquare_v2.duckdb_extension.wasm
<repo>/index.json
```

One build per platform serves every DuckDB release (C API struct v1.2.0), so the script
copies it under each version folder; rerun the workflow when DuckDB releases a new
version so its folder exists. Host the folder on S3/CloudFront/R2 or any static server.
For DuckDB-Wasm the host must send `Access-Control-Allow-Origin` (CORS).

```sql
SET allow_unsigned_extensions = true;   -- the build is not signed by DuckDB
INSTALL geosquare_v2 FROM 'https://<host>/<prefix>';
LOAD geosquare_v2;
```

DuckDB-Wasm: open the database with `allowUnsignedExtensions: true`, then run the same
`INSTALL ... FROM` and `LOAD`.

Tested locally: INSTALL + LOAD from a repository built by the script on DuckDB 1.4.0,
1.4.5, 1.5.0, 1.5.6, 2.0.0 and DuckDB-Wasm 2.0.0 in Chromium.

Workflow settings: repository variables `EXTENSION_REPO_BUCKET`, `EXTENSION_REPO_PREFIX`,
`EXTENSION_REPO_REGION` and secrets `EXTENSION_REPO_AWS_ACCESS_KEY_ID`,
`EXTENSION_REPO_AWS_SECRET_ACCESS_KEY`. A tag `duckdb-v<version>` builds, tests, attaches
the repository tarball to a GitHub release and syncs it to the bucket.

## 2. DuckDB community extensions (signed, `INSTALL geosquare_v2 FROM community`)

The community CI builds and signs extensions from a public repository whose root follows
`duckdb/extension-template-rs`. Steps:

1. Create a public repository (draft name `geosquareai/duckdb-geosquare-v2`) with this
   folder at the root, the `extension-ci-tools` submodule, and the core crate vendored or
   published (`geosquare-engine` is private).
2. Point `repo.ref` in `description.yml` at a commit and open a pull request to
   `duckdb/community-extensions` adding `extensions/geosquare_v2/description.yml`.

Making code public and opening that pull request are owner decisions; nothing here does it.
