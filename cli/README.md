# gsq

`gsq` is the command-line interface for the GeoSquare V2 DuckDB extension. It
loads the extension supplied with `--extension PATH` or `GSQ_EXTENSION` and
keeps grid validation and geometry work inside DuckDB.

## Install

Build the extension first:

```zsh
cd bindings/duckdb
DUCKDB_PLATFORM=osx_arm64 ./build.sh
```

Install the CLI in a virtual environment or another Python environment:

```zsh
python -m pip install ./cli
```

Every command needs the extension path, either globally or after the command:

```zsh
export GSQ_EXTENSION="$PWD/bindings/duckdb/build/osx_arm64/geosquare_v2.duckdb_extension"
gsq --help
```

## Commands

### Detect columns

`detect` accepts CSV or Parquet input and reports the engine's column
classification. The default is a readable table; `--format json` emits JSON.

```zsh
gsq detect points.csv --domain ID --level 8
gsq detect points.parquet --format json --max-rows 10000
```

### Roll up cells

`rollup` accepts a CSV with a `gid` column or a cell dataset directory. `--level`
must not be greater than the input level. The output format follows the `.csv`
or `.parquet` extension.

```zsh
gsq rollup population.csv --level 6 --sum population --out population-level6.csv
gsq rollup cells/ --level 8 --out summary.parquet
```

The result contains `parent_gid`, `cells`, and one sum column for each repeated
`--sum` option. Parent IDs are the first `L` characters of each child GID.

### Join cell tables

`join` performs an inner or left join on `gid` or `packed_id`. Both inputs must
contain one valid grid level, and the levels must match.

```zsh
gsq join left.csv right.parquet --on gid --how left --out joined.parquet
```

### Convert cells to geometry

`to-geo` writes a GeoJSON FeatureCollection when the output ends in `.geojson`.
Other outputs are CSV with a `wkt` column. The default output CRS is WGS84;
pass `--crs grid` to keep the engine's grid coordinates.

```zsh
gsq to-geo cells.csv --domain ID --out cells.geojson
gsq to-geo cells.csv --domain ID --out cells.csv --crs EPSG:4326
```

KH, MM, TH, TL, and VN are candidate CRS domains. They require
`--allow-candidate-crs`, which is passed explicitly to the extension and does
not change their support status.

### Check a file before upload

`check` reads a CSV or Parquet file with a `gid` column and reports:

- GIDs that are not valid for `--domain`. Row numbers count data rows from 1.
- GIDs that appear more than once.
- More than one grid level.
- With `--source` and `--value`, a difference between the column totals of
  the two files.

`--domain` is required because a bare GID does not name its domain.

```zsh
gsq check cells.csv --domain ID
gsq check cells.csv --domain ID --source source.csv --value population --json
```

The command exits 0 when no problem is found and 1 when it finds one. An empty
file counts as a problem. `--json` prints the full report on stdout.

### Export GeoParquet and a Kepler.gl map

`export` writes the cells as GeoParquet. Each row becomes one polygon in a
`geometry` column (WKB, WGS84). All other columns are kept. The file opens
directly in QGIS, DuckDB, and GeoPandas.

The input must pass the same GID check as `check`. Bad input is refused and no
file is written. `--out` must end in `.parquet`. Candidate domains need
`--allow-candidate-crs`.

```zsh
gsq export cells.csv --domain ID -o cells.parquet
gsq export cells.csv --domain ID -o cells.parquet --kepler cells.kepler.json --color population
```

`--kepler` writes a Kepler.gl map config with one GeoJSON layer on `geometry`.
Load the Parquet file in Kepler.gl with the dataset name equal to the file name
without `.parquet` (for example `cells`), then import the config. `--color`
colors the layer by that column and needs `--kepler`.

### Cell pyramid

`pyramid` writes a base level 12 table as a pyramid: levels 12, 10 and 8. A map
reads the level that fits its zoom and only the files on screen. The output is
static files plus `pyramid.json`, the index.

```zsh
gsq pyramid cells12.parquet --domain ID -o out/pyramid \
  --value population=sum --value unemployment=weighted_mean:population \
  --value flood_m=max --share-column share_in_aoi
```

- `--value NAME=RULE` declares a value column. Rules: `sum`, `max`, `min`, `median`
  (needs 3 values, else null), and `weighted_mean:WEIGHT`. The weight must be a
  `sum` column, or `area` (every cell weighs the same).
- Coarser levels group the base rows by parent GID. A parent value is the rule
  applied to its children. Nulls are skipped.
- `--share-column` holds the share of each cell inside the area, a number in
  [0, 1]. A parent share is the sum of its children's shares divided by 100 (level
  10) or 10,000 (level 8).
- `-o` must be a new folder. The write goes to a temporary folder first, then gets
  renamed. A failed run leaves no folder behind.

Spec notes (from `docs/cell-pyramid.md` on geosquare-v2 PR #33):

- The spec example shows profile `2.1.0-static-datum`. The writer uses the core
  value `2.2.0-explicit-datum`, from `core/src/domain.rs`.
- A CSV or Parquet input has no manifest. So `source.manifest_version` is `null`.
- Every value column is written as `double`.
- `share_sha256` is `null` when there is no `--share-column`.
- Files are written one per non-empty partition, and each is read back to hash it.
  Speed and file size on national data are not measured yet.

## Errors and output safety

Usage errors exit with status 2. Data and validation errors exit with status 1
and are written to stderr. File outputs are written to a temporary file in the
output directory and renamed only after the DuckDB operation succeeds.

# Bahasa Indonesia

`gsq` adalah CLI untuk ekstensi DuckDB GeoSquare V2. Path ekstensi bisa diberikan
lewat `--extension PATH` atau environment variable `GSQ_EXTENSION`.

Perintahnya adalah `detect` untuk mendeteksi kolom, `rollup` untuk merangkum sel
ke level yang lebih rendah, `join` untuk menggabungkan dua tabel sel, dan
`to-geo` untuk membuat GeoJSON atau kolom WKT. Semua validasi grid dan hitungan
geometri dilakukan oleh ekstensi DuckDB, bukan dihitung ulang di Python.

`rollup`, `join`, dan `to-geo` menulis ke file CSV atau Parquet secara atomik.
`to-geo` memakai WGS84 secara default; gunakan `--crs grid` bila koordinat grid
diperlukan. Domain kandidat KH, MM, TH, TL, dan VN wajib memakai
`--allow-candidate-crs`.

`check` memeriksa file CSV atau Parquet sebelum upload. Perintah ini melaporkan
GID yang tidak valid untuk `--domain`, GID kembar, level yang bercampur, dan
selisih total kolom bila `--source` dan `--value` diberikan. Exit code 0 berarti
tidak ada masalah, 1 berarti ada masalah.

`export` menulis sel ke GeoParquet. Setiap baris jadi satu poligon di kolom
`geometry` (WKB, WGS84). Semua kolom lain tetap ada. File ini bisa langsung
dibuka di QGIS, DuckDB, dan GeoPandas. Input wajib lolos cek GID yang sama
dengan `check`. Input yang buruk ditolak dan tidak ada file yang ditulis.
`--out` harus berakhiran `.parquet`.

`--kepler` menulis konfigurasi peta Kepler.gl dengan satu layer GeoJSON di
kolom `geometry`. Nama dataset di Kepler.gl adalah nama file tanpa `.parquet`.
`--color` mewarnai layer menurut kolom itu dan butuh `--kepler`.

### Piramida sel

`pyramid` menulis tabel level 12 sebagai piramida: level 12, 10, dan 8. Peta
membaca level yang cocok dengan zoom, dan hanya file yang ada di layar. Hasilnya
berupa file statis dan `pyramid.json` sebagai indeks. Contoh perintah ada di
bagian Inggris di atas.

- `--value NAME=RULE` mendaftar kolom nilai. Aturan: `sum`, `max`, `min`,
  `median` (butuh 3 nilai, kalau tidak hasilnya kosong), dan `weighted_mean:BOBOT`.
  Bobot harus kolom `sum`, atau `area`.
- Level yang lebih kasar mengelompokkan baris level 12 per GID induk. Nilai induk
  dihitung dari anaknya. Nilai kosong dilewati.
- `--share-column` berisi bagian sel yang ada di dalam area, angka 0 sampai 1.
  Share induk adalah jumlah share anaknya dibagi 100 (level 10) atau 10.000 (level 8).
- `-o` harus folder baru. Hasilnya ditulis dulu ke folder sementara. Kalau gagal,
  tidak ada folder yang tertinggal.
