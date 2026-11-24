# geosquare-engine (JavaScript / WebAssembly)

Binding WebAssembly untuk core GeoSquare V2. Dipakai di browser dan Node.js.

Status: codec, packing, topologi, dan detector sudah ada. Semua vector spec 2.0 yang tidak butuh CRS lulus di JS.

## Pakai

```js
import init, { encode, decode, pack, unpack, kRing, detectTable } from "geosquare-engine";

await init(); // sekali saja, sebelum fungsi lain dipanggil

const gid = encode(12, 475168, 491792);  // "H2X545M5X4E2"
const cell = decode(gid);                // { level: 12, xIdx: 475168, yIdx: 491792 }
const packed = pack(1, gid);             // BigInt
unpack(packed);                          // { domainId: 1, gid: "H2X545M5X4E2" }
kRing(gid, 1);                           // 8 tetangga
```

## Aturan

- Packed ID selalu `BigInt`. `Number` ditolak. Ini mencegah nilai Int64 kehilangan presisi.
- Angka harus bilangan bulat dan ada dalam rentang. `256` untuk level, `2.5` untuk indeks, atau `null` akan melempar error. Tidak ada yang dipotong atau dibulatkan diam-diam.
- `BigInt` di luar Int64 melempar error. Tidak dipotong.
- `detectTable` menerima `string`, `BigInt`, angka bulat sampai 2^53 - 1, dan `null`. Nilai lain melempar error.

## Membangun

```zsh
cargo install wasm-bindgen-cli --version 0.2.130 --locked   # sekali saja, versi harus sama dengan Cargo.toml
npm run build
npm test
npm run pack:check
```
## Geographic cell operations

The CRS-aware exports use a numeric registry domain ID and keep the core CRS gate:

```js
const gid = pointToCell(1, 118.01521445, -2.55029565, 5, false);
const ring = cellPolygonWgs84(1, gid, 25000, false);
```

- `pointToCell(domainId, longitude, latitude, level, allowCandidateCrs)` returns a GID.
- `cellPolygonWgs84(domainId, gid, maxSegmentLengthM, allowCandidateCrs)` returns a closed array of `[longitude, latitude]` pairs.
- `domainId`, coordinates, level, and segment length are checked JavaScript `Number` values. Invalid, non-finite, fractional, or out-of-range values throw.
- Released domains use `false` for `allowCandidateCrs`. Candidate domains are refused unless that final argument is explicitly `true`.

## Rollup zoom (`geosquare-engine/rollup`)

Gabung baris dari level halus ke sel induk di level yang lebih kasar. Dipakai untuk peta
yang di-zoom jauh. Parent dihitung oleh `parent` dari core Rust, jadi aturan grid tidak
disalin ke JS.

```js
import init from "geosquare-engine";
import { levelForZoom, rollupToLevel } from "geosquare-engine/rollup";

await init(); // sekali saja, sebelum rollup dipanggil

const level = levelForZoom(map.getZoom()); // zoom 14 ke atas -> 12, 11-13 -> 10, 10 ke bawah -> 8
const out = rollupToLevel(rows, level, {
  penduduk: { rule: "sum" },
  pengangguran: { rule: "weighted_mean", weight: "penduduk" },
  harga_m2: { rule: "median" },
  kedalaman: { rule: "max" },
});
// out: [{ gid, penduduk, pengangguran, ... }], satu baris per GID induk, urut GID
```

- `rows`: `[{ gid, kolom: angka | null }]`. Baris boleh di level mana saja yang lebih halus dari `level`.
- Aturan: `sum`, `weighted_mean`, `median`, `max`, `min`. Sama dengan rollup DuckDB.
- `majority_weighted` (kategori, bobot terbesar) direncanakan. Belum ada di server, jadi belum ada di sini. Aturan tidak dikenal melempar error.
- `weight` berisi nama kolom, atau `"area"` (bobot 1 per sel, karena sel di satu level punya luas sama).
- `weighted_mean` wajib punya `weight`. `sum`, `median`, `max`, `min` tidak boleh punya `weight`.
- NULL dilewati, tidak dihitung 0. Induk tanpa nilai valid hasilnya `null`.
- `median` butuh minimal 3 nilai.
- Input yang salah melempar error: aturan tidak dikenal, nilai bukan angka hingga, level lebih halus dari baris.
- `levelForZoom` hanya mengembalikan level yang ada di pyramid (12, 10, 8). Zoom 14 ke atas: 12.
  Zoom 11 sampai 13: 10. Zoom 10 ke bawah: 8. Ini jawaban untuk spec pyramid bagian 11 nomor 8.
  Ambang ini perkiraan dan belum dituning di peta nyata. Level tidak dibatasi ke level dataset; batasi sendiri dengan `Math.min`.

## Layer deck.gl (`geosquare-engine/deck`)

`GeoSquareLayer` dipakai seperti `H3HexagonLayer`. Berikan baris dengan `gid` dan nilai.
Layer membuat poligon sel dari core Rust. deck.gl adalah peer dependency, jadi
aplikasi harus memasangnya sendiri (`@deck.gl/core` dan `@deck.gl/layers`, versi 9.4 atau lebih baru).

```js
import { Deck } from "@deck.gl/core";
import init, { pointToCell } from "geosquare-engine";
import { GeoSquareLayer } from "geosquare-engine/deck";

await init();
const rows = [{ gid: pointToCell(1, 106.8, -6.2, 12, false), value: 3 }];
new Deck({ initialViewState: { longitude: 106.8, latitude: -6.2, zoom: 14 }, controller: true,
  layers: [new GeoSquareLayer({ id: "cells", data: rows, domainId: 1 })] });
```

- `domainId`: wajib. Domain registry dari GID, misalnya 1 untuk Indonesia.
- `zoom` dan `aggregation`: isi keduanya atau tidak sama sekali. Dengan keduanya, baris digabung ke
  level untuk zoom itu, dengan aturan seperti `{ rule: "sum" }`. Tanpa keduanya, tiap baris jadi satu sel.
- `getGid`, `getValue`, `getFillColor`, `maxSegmentLengthM`: bisa diubah. Default warna adalah warna tetap.
- Contoh lengkap: `examples/deck-basic.html`. Jalankan dengan server web lokal.
- Shortcut: poligon dihitung ulang setiap kali layer dirender. Tidak ada cache per GID.
