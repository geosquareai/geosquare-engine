# geosquare-engine (JavaScript / WebAssembly)

WebAssembly binding for the GeoSquare V2 core. Use it in the browser and in Node.js.

Status: the codec, packing, topology, detector, cell geometry, rollup, and deck.gl layer are done. All spec 2.0 vectors that do not need CRS pass in JS. The npm package is not published yet. Build it from this repo (see [Build](#build)).

## Use

```js
import init, { encode, decode, pack, unpack, kRing, detectTable } from "geosquare-engine";

await init(); // call once, before any other function

const gid = encode(12, 475168, 491792);  // "H2X545M5X4E2"
const cell = decode(gid);                // GridCell: cell.level 12, cell.xIdx 475168, cell.yIdx 491792
const packed = pack(1, gid);             // BigInt
unpack(packed);                          // GridId: domainId 1, gid "H2X545M5X4E2"
kRing(gid, 1);                           // 8 neighbours
```

## Exports

| Export | What it does |
|---|---|
| `encode(level, xIdx, yIdx)` | Make a GID from a cell's indices. Returns `string` |
| `decode(gid)` | Read the level and indices of a GID. Returns a `GridCell` with `level`, `xIdx`, `yIdx` |
| `pack(domainId, gid)` | Join a domain ID and a GID into one Int64. Returns `BigInt` |
| `unpack(packed)` | Split a packed `BigInt` back into a `GridId` with `domainId` and `gid` |
| `parent(gid)` | Parent cell GID. Returns `string`, or `undefined` for the root |
| `children(gid)` | Direct children. Returns `string[]` |
| `descendantRange(domainId, parentGid, childLevel)` | Int64 range that covers the descendants at a level. Some values inside the range are not valid codes |
| `kRing(gid, k)` | Cells at exactly `k` steps away. Returns `string[]` |
| `kDisk(gid, k)` | Cells at most `k` steps away, including the cell itself. Returns `string[]` |
| `distanceM(a, b)` | Distance in metres between two cells at the same level |
| `cellSideM(level)` | Side length of a cell at a level, in metres |
| `pointToCell(domainId, longitude, latitude, level, allowCandidateCrs)` | GID of the cell that holds a WGS 84 point |
| `cellPolygonWgs84(domainId, gid, maxSegmentLengthM, allowCandidateCrs)` | Closed ring of `[longitude, latitude]` pairs for a cell |
| `detectTable(columns, domain, level)` | Finds which columns hold GIDs, packed IDs, or indices. `columns` is an object of `(dtype, values)`. Same detector as the Python and DuckDB bindings |
| `version()` | Version of the engine core. Returns `string` |

Browser and Node code can both use these. The JS modules below run on top of these exports.

### Rollup: `geosquare-engine/rollup`

Use this to combine fine rows into their parent cells at a coarser level. Use it when a map is zoomed out. The parent is calculated by the Rust core, so the grid rules are not copied into JS.

```js
import init from "geosquare-engine";
import { levelForZoom, rollupToLevel } from "geosquare-engine/rollup";

await init(); // call once, before rollupToLevel

const level = levelForZoom(map.getZoom()); // zoom 14 and up -> 12, zoom 11 to 13 -> 10, zoom 10 and below -> 8
const out = rollupToLevel(rows, level, {
  penduduk: { rule: "sum" },
  pengangguran: { rule: "weighted_mean", weight: "penduduk" },
  harga_m2: { rule: "median" },
  kedalaman: { rule: "max" },
});
// out: [{ gid, penduduk, pengangguran, ... }], one row per parent GID, sorted by GID
```

- `rows`: `[{ gid, columnName: number | null }]`. Rows can be at any level finer than `level`.
- Rules: `sum`, `weighted_mean`, `median`, `max`, `min`. They match the DuckDB rollup.
- `weighted_mean` needs `weight`. The `weight` is a column name, or `"area"` (every cell weighs 1, because cells at one level have the same area).
- `sum`, `median`, `max`, and `min` do not take `weight`.
- `median` needs at least 3 values.
- NULL values are skipped. They are not counted as 0. A parent with no valid value gets `null`.
- Unknown rules and bad values throw an error. This includes a level finer than the rows.
- `levelForZoom` returns only levels the pyramid holds: 12, 10, or 8.
  shortcut: the zoom thresholds are approximate and not tuned on real maps. Upgrade by tuning them on real map data. The result is not capped to the dataset level. Cap it yourself with `Math.min`.
- `majority_weighted` is planned. It is not implemented yet, so an unknown rule throws.

### Cells: `geosquare-engine/cells`

`cellPolygons(rows, options)` turns GID rows into cell rings for a map. It returns `[{ gid, value, polygon }]`.

```js
import { cellPolygons } from "geosquare-engine/cells";

const items = cellPolygons(rows, {
  domainId: 1,
  getGid: (row) => row.gid,
  getValue: (row) => row.value,
  maxSegmentLengthM: 1000,
  zoom: map.getZoom(),                       // optional: set together with aggregation
  aggregation: { rule: "sum" },
});
```

- `domainId` is required. It is the registry domain of the GIDs, for example `1` for Indonesia.
- `zoom` and `aggregation` go together. Set both or neither.
- shortcut: there is no cache. A big dataset rebuilds every ring on each render. Add a cache keyed by GID when the map redraws often.

### deck.gl layer: `geosquare-engine/deck`

`GeoSquareLayer` works like `H3HexagonLayer`. Pass rows with a `gid` and a value. The layer builds the cell polygons with the Rust core.

deck.gl is a peer dependency. Your app must install `@deck.gl/core` and `@deck.gl/layers`, version 9.4 or newer.

```js
import { Deck } from "@deck.gl/core";
import init, { pointToCell } from "geosquare-engine";
import { GeoSquareLayer } from "geosquare-engine/deck";

await init();
const rows = [{ gid: pointToCell(1, 106.8, -6.2, 12, false), value: 3 }];
new Deck({
  initialViewState: { longitude: 106.8, latitude: -6.2, zoom: 14 },
  controller: true,
  layers: [new GeoSquareLayer({ id: "cells", data: rows, domainId: 1 })],
});
```

- `domainId` is required.
- `zoom` and `aggregation` go together. With both, rows roll up to the level for that zoom. Without them, each row is one cell.
- `getGid`, `getValue`, `getFillColor`, and `maxSegmentLengthM` can be changed. The default colour is one fixed colour.
- A full example is in `examples/deck-basic.html`. Serve this folder with a local web server.
- shortcut: the polygons are rebuilt on every render. There is no cache per GID.

## Rules

- A packed ID is always a `BigInt`. A `Number` is refused. This stops Int64 values from losing precision.
- Numbers must be whole and in range. `256` for a level, `2.5` for an index, or `null` throws an error. Nothing is cut or rounded silently.
- A `BigInt` outside Int64 throws an error. It is not cut.
- `detectTable` accepts `string`, `BigInt`, whole numbers up to 2^53 - 1, and `null`. Other values throw an error.
- `pointToCell` and `cellPolygonWgs84` take a numeric `domainId`, and their coordinates, level, and segment length are checked `Number` values. Invalid, non-finite, fractional, or out-of-range values throw.
- Released domains use `false` for `allowCandidateCrs`. Candidate domains (KH, MM, TH, TL, VN) are refused unless the last argument is `true`. Passing `true` does not make a domain released.

## Build

Build the package from this folder. The build needs Rust and `wasm-bindgen-cli`:

```zsh
cargo install wasm-bindgen-cli --version 0.2.130 --locked   # once. The version must match Cargo.toml
npm run build                                               # writes pkg/
npm test
npm run pack:check
```

The `pkg/` folder is not in git. The build creates it.

## Files

| Path | What it is |
|---|---|
| `pkg/` | WebAssembly build and JS glue (made by `npm run build`) |
| `js/rollup.mjs` | Zoom rollup and zoom-to-level helper |
| `js/cells.mjs` | Cell rings for a map |
| `js/deck.mjs` | deck.gl layer |
| `examples/deck-basic.html` | Example page |

## Licence

Apache-2.0. See the repository `LICENSE`.

---

# Bahasa Indonesia

Binding WebAssembly untuk core GeoSquare V2. Dipakai di browser dan Node.js.

Status: codec, packing, topologi, detector, geometri sel, rollup, dan layer deck.gl sudah ada. Semua vector spec 2.0 yang tidak butuh CRS lulus di JS. Paket npm belum dipublikasikan. Bangun dari repo ini (lihat bagian Build di atas).

## Pakai

```js
import init, { encode, decode, pack, unpack, kRing, detectTable } from "geosquare-engine";

await init(); // sekali saja, sebelum fungsi lain dipanggil

const gid = encode(12, 475168, 491792);  // "H2X545M5X4E2"
const cell = decode(gid);                // cell.level 12, cell.xIdx 475168, cell.yIdx 491792
const packed = pack(1, gid);             // BigInt
unpack(packed);                          // domainId 1, gid "H2X545M5X4E2"
kRing(gid, 1);                           // 8 tetangga
```

## Export

Daftar export ada di tabel bagian English di atas. Intinya:

- `encode`, `decode`: buat dan baca GID.
- `pack`, `unpack`: gabung dan pisah domain ID dengan GID. Hasilnya `BigInt`.
- `parent`, `children`, `kRing`, `kDisk`, `descendantRange`: relasi antar sel.
- `distanceM`, `cellSideM`: jarak dan ukuran sel dalam meter.
- `pointToCell`, `cellPolygonWgs84`: titik WGS 84 ke sel, dan sel ke poligon.
- `detectTable`, `version`: deteksi kolom dan versi core.
- `geosquare-engine/rollup`: `levelForZoom` dan `rollupToLevel`.
- `geosquare-engine/cells`: `cellPolygons`.
- `geosquare-engine/deck`: `GeoSquareLayer` untuk deck.gl.

## Aturan

- Packed ID selalu `BigInt`. `Number` ditolak. Ini mencegah nilai Int64 kehilangan presisi.
- Angka harus bilangan bulat dan ada dalam rentang. `256` untuk level, `2.5` untuk indeks, atau `null` akan melempar error. Tidak ada yang dipotong atau dibulatkan diam-diam.
- `BigInt` di luar Int64 melempar error. Tidak dipotong.
- Domain yang sudah released memakai `false` untuk `allowCandidateCrs`. Domain kandidat (KH, MM, TH, TL, VN) ditolak kecuali argumen terakhir `true`. Nilai `true` tidak membuat domain itu released.

## Rollup zoom (`geosquare-engine/rollup`)

Gabungkan baris dari level halus ke sel induk di level yang lebih kasar. Dipakai saat peta di-zoom jauh. Induk dihitung oleh core Rust, jadi aturan grid tidak disalin ke JS.

- `levelForZoom`: zoom 14 ke atas jadi level 12. Zoom 11 sampai 13 jadi level 10. Zoom 10 ke bawah jadi level 8. Ambang ini perkiraan, belum di-tuning di peta nyata.
- `rollupToLevel`: aturan `sum`, `weighted_mean`, `median`, `max`, dan `min`. Sama dengan rollup DuckDB.
- `weighted_mean` wajib punya `weight`. `weight` bisa nama kolom atau `"area"`.
- NULL dilewati, tidak dihitung 0. Induk tanpa nilai valid hasilnya `null`.
- `median` butuh minimal 3 nilai.
- `majority_weighted` belum ada. Aturan yang tidak dikenal melempar error.

## Layer deck.gl (`geosquare-engine/deck`)

`GeoSquareLayer` dipakai seperti `H3HexagonLayer`. Beri baris dengan `gid` dan nilai. deck.gl adalah peer dependency. Aplikasi harus memasang `@deck.gl/core` dan `@deck.gl/layers` versi 9.4 atau lebih baru.

- `domainId` wajib diisi. Contoh: `1` untuk Indonesia.
- `zoom` dan `aggregation` diisi bersama, atau tidak sama sekali.
- Contoh lengkap ada di `examples/deck-basic.html`. Jalankan dengan server web lokal.
- Shortcut: poligon dihitung ulang setiap render. Belum ada cache per GID.

## Build

```zsh
cargo install wasm-bindgen-cli --version 0.2.130 --locked   # sekali saja. Versinya harus sama dengan Cargo.toml
npm run build                                               # membuat pkg/
npm test
npm run pack:check
```

Folder `pkg/` tidak ada di git. Build akan membuatnya.

## Lisensi

Apache-2.0. Lihat `LICENSE` di root repo.
