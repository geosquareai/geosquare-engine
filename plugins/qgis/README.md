# GeoSquare E14 QGIS plugin v1

This internal-analysis plugin turns a GeoSquare V2 GID column into an in-memory
polygon layer and rolls numeric attributes up to an ancestor level. It writes no
files and makes no network calls. v1 does not implement k-anonymity. The decision
about k-anonymity remains open in `plan/10-status-fase4.md`.

## Actions

- **Cells from GID field** asks for the active vector layer's GID field and grid
a domain. It validates every GID with the engine, requires one level across the
layer, creates WGS84 polygons, and copies numeric fields to the output features.
An invalid GID, mixed level, or invalid numeric value prevents the output layer
from being added.
- **Rollup to level** works on a layer created by the first action. It walks each
GID upward with the engine, groups by the selected ancestor level, and sums the
numeric fields. The result is another in-memory EPSG:4326 layer.

The plugin deliberately exposes candidate CRS domains as an explicit technical
choice. The engine still applies its candidate-CRS opt-in rules.

## Install the Python package in QGIS

The plugin needs the `geosquare-engine` Python package in QGIS's own Python
runtime. Installing it into another virtual environment is not enough.

For a released package:

```zsh
QGIS_PYTHON=/Applications/QGIS.app/Contents/MacOS/bin/python3
"$QGIS_PYTHON" -m pip install geosquare-engine
```

For a local engine checkout, build a wheel outside QGIS and install that wheel
with the same QGIS Python:

```zsh
cd bindings/python
uv venv .venv
uv pip install --python .venv/bin/python maturin
.venv/bin/maturin build --release
QGIS_PYTHON=/Applications/QGIS.app/Contents/MacOS/bin/python3
"$QGIS_PYTHON" -m pip install target/wheels/geosquare_engine-*.whl
```

Do not bundle Rust or Python binaries in this plugin.

## Install the plugin

Copy the contents of this directory to a QGIS Python plugin directory with a
non-conflicting package name such as `geosquare_e14`, then enable **GeoSquare
E14** in **Plugins > Manage and Install Plugins > Installed**:

```text
~/Library/Application Support/QGIS/QGIS3/profiles/default/python/plugins/geosquare_e14
```

Do not install it as a directory named `qgis`: that name conflicts with QGIS's
own Python package. The final directory must contain `metadata.txt` and
`classFactory` from `__init__.py`. QGIS itself must be tested by the user in its
GUI. The repository contains pure-logic pytest coverage, but this plugin does
not open the QGIS GUI in automated tests.

---

# Bahasa Indonesia

Plugin ini untuk analisis internal. Plugin membuat layer polygon WGS84 di memori
dari kolom GID, lalu menjumlahkan kolom numerik ke level induk. Plugin tidak
menulis file, tidak mengakses jaringan, dan belum menerapkan k-anonimitas.
Pertanyaan k-anonimitas masih terbuka di `plan/10-status-fase4.md`.

Paket Python `geosquare-engine` harus dipasang di Python milik QGIS, bukan hanya
di virtual environment lain. Instal paketnya, salin folder plugin ke folder
plugin QGIS, lalu aktifkan **GeoSquare E14** dari menu plugin QGIS.
