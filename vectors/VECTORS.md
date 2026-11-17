# Test vector spec 2.0

Folder `spec-2.0/` adalah salinan dari `geosquare-v2` pada tag `spec-2.0` (commit `93f1d45`).

Jangan edit file di sini dengan tangan. Kalau ada perubahan, ubah di `geosquare-v2`, buat tag baru, lalu salin ulang.

## Cara cek salinan

```zsh
cd vectors/spec-2.0
python3 - <<'PY'
import hashlib, json
m = json.load(open("manifest.json"))
for name, entry in m["files"].items():
    data = open(name, "rb").read()
    assert hashlib.sha256(data).hexdigest() == entry["sha256"], name
print("ok", len(m["files"]), "files")
PY
```

## Isi

| File | Isi |
|---|---|
| `codec.json` | GID, Int64, dan kasus tolak untuk level 0 sampai 14. Juga packed ID untuk 11 domain |
| `topology.json` | Parent, child, range keturunan (per domain), tetangga, dan jarak |
| `detect.json` | Kasus deteksi kolom |
| `projected.json` | Aturan tepi grid CRS. Belum dipakai, menunggu fase CRS |
| `points.json` | Lon/lat ke GID per domain. Belum dipakai, menunggu fase CRS |
| `manifest.json` | Versi spec, versi registry, dan SHA-256 tiap file |
