# geosquare-engine (Python)

Binding Python untuk core GeoSquare V2. Isinya sama dengan `core/` di Rust, dibungkus PyO3.

Status: codec, packing, topologi, dan detector sudah ada. Semua vector spec 2.0 yang tidak butuh CRS lulus di Python.

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

## Aturan

- Input salah selalu melempar error. Tidak ada nilai yang diperbaiki diam-diam.
- `detect_table` menerima `str`, `int`, dan `None`. Float atau bool melempar `TypeError`. Ubah NaN jadi `None` dulu.
- Kolom `int64` yang nilainya di luar rentang Int64 melempar `OverflowError`.
- Hasil harus sama dengan `vectors/spec-2.0`. Test ada di `tests/test_vectors.py`.

## Uji file nyata

```zsh
GEOSQUARE_REAL_DATA_ROOT=/Users/sawungrana/Documents/map/geosquare \
  .venv/bin/python -m pytest -q tests/test_real_files.py
```

Folder yang dipakai harus berisi `geosquare-risk/`, `pandaigadai/`, dan `dukcapil/`. Tanpa variabel itu, test ini dilewati. Butuh `pyarrow`.
