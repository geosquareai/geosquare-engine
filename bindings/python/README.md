# geosquare-engine (Python)

Python binding for the GeoSquare V2 core. It has the same code as `core/` in Rust, wrapped with PyO3.

Status: codec, packing, topology, detector, and cell-to-WGS84 polygon operations are done. Every spec 2.0 vector passes in Python.

## Install

```zsh
pip install geosquare-engine
```

## Develop

```zsh
cd bindings/python
uv venv .venv
uv pip install --python .venv/bin/python maturin pytest
.venv/bin/maturin develop
.venv/bin/python -m pytest -q
```

## Example

```python
import geosquare_engine as gs

gid = gs.encode(12, 475168, 491792)   # "H2X545M5X4E2"
gs.decode(gid)                         # (12, 475168, 491792)
packed = gs.pack(1, gid)               # Int64, domain ID 1
gs.unpack(packed)                      # (1, "H2X545M5X4E2")
gs.k_ring(gid, 1)                      # 8 neighbours, not including the cell itself
gs.distance_m(gid, gs.k_ring(gid, 1)[0])
ring = gs.cell_polygon_wgs84(1, gid, 1000.0)  # closed WGS84 (lon, lat) ring
```

`cell_polygon_wgs84` needs the numeric domain ID because a bare GID does not
contain a domain. Its segment-length argument is in metres. Candidate CRS domains
also require `allow_candidate_crs=True` explicitly.

## pandas and Polars

Import one module to add the `gsq` accessor. pandas and Polars are optional. Install them yourself.

```python
import geosquare_engine.pandas_gsq   # or geosquare_engine.polars_gsq
df.gsq.parent(9)                     # parent GID at level 9
df.gsq.ring(1)                       # neighbours at distance 1, as a list per row
df.gsq.level()                       # GID level
df.gsq.to_geo(domain_id=1)           # cell polygons (GeoDataFrame if geopandas is installed)
```

The GID column is found by the detector. Pass `column="name"` when the column is short (under 5 characters) or the table has more than one GID column. Invalid GIDs raise `ValueError`. Missing values stay missing.

## Rules

- Invalid input always raises an error. Nothing is repaired silently.
- `detect_table` accepts `str`, `int`, and `None`. A float or bool raises `TypeError`. Turn NaN into `None` first.
- An `int64` column value outside the Int64 range raises `OverflowError`.
- Results must match `vectors/spec-2.0`. The tests are in `tests/test_vectors.py` and `tests/test_geo.py`.

## Real file tests

```zsh
GEOSQUARE_REAL_DATA_ROOT=/path/to/data \
  .venv/bin/python -m pytest -q tests/test_real_files.py
```

The folder must contain `geosquare-risk/`, `pandaigadai/`, and `dukcapil/`. Without that variable, the test is skipped. It needs `pyarrow`.

---

# Bahasa Indonesia

Binding Python untuk core GeoSquare V2. Isinya sama dengan `core/` di Rust, dibungkus PyO3.

Status: codec, packing, topologi, detector, dan operasi polygon sel ke WGS84 sudah ada. Semua vector spec 2.0 lulus di Python.

## Pasang untuk pengembangan

```zsh
cd bindings/python
uv venv .venv
uv pip install --python .venv/bin/python maturin pytest
.venv/bin/maturin develop
.venv/bin/python -m pytest -q
```

## Contoh

```python
import geosquare_engine as gs

gid = gs.encode(12, 475168, 491792)   # "H2X545M5X4E2"
gs.decode(gid)                         # (12, 475168, 491792)
packed = gs.pack(1, gid)               # Int64, domain ID 1
gs.unpack(packed)                      # (1, "H2X545M5X4E2")
gs.k_ring(gid, 1)                      # 8 tetangga, tanpa sel itu sendiri
gs.distance_m(gid, gs.k_ring(gid, 1)[0])
```

## pandas dan Polars

Impor satu modul untuk menambah accessor `gsq`. pandas dan Polars bersifat opsional. Pasang sendiri.

```python
import geosquare_engine.pandas_gsq   # atau geosquare_engine.polars_gsq
df.gsq.parent(9)                     # GID induk di level 9
df.gsq.ring(1)                       # tetangga jarak 1, berupa daftar per baris
df.gsq.level()                       # level GID
df.gsq.to_geo(domain_id=1)           # poligon sel (GeoDataFrame jika geopandas terpasang)
```

Kolom GID dicari dengan detektor. Isi `column="nama"` jika GID-nya pendek (di bawah 5 karakter) atau tabel punya lebih dari satu kolom GID. GID yang salah memunculkan `ValueError`. Nilai kosong tetap kosong.

## Aturan

- Input salah selalu melempar error. Tidak ada nilai yang diperbaiki diam-diam.
- `detect_table` menerima `str`, `int`, dan `None`. Float atau bool melempar `TypeError`. Ubah NaN jadi `None` dulu.
- Kolom `int64` yang nilainya di luar rentang Int64 melempar `OverflowError`.
- Hasil harus sama dengan `vectors/spec-2.0`. Test ada di `tests/test_vectors.py`.

## Uji file nyata

```zsh
GEOSQUARE_REAL_DATA_ROOT=/path/to/data \
  .venv/bin/python -m pytest -q tests/test_real_files.py
```

Folder yang dipakai harus berisi `geosquare-risk/`, `pandaigadai/`, dan `dukcapil/`. Tanpa variabel itu, test ini dilewati. Butuh `pyarrow`.
