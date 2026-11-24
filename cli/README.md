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
