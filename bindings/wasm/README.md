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
