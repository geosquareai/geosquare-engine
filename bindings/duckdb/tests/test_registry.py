"""Registry signature verification tests.

Run with a built extension:

    python tests/test_registry.py build/osx_arm64/geosquare_v2.duckdb_extension
"""

import hashlib
import json
import sys
import tempfile
from pathlib import Path

import duckdb

EXT = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux_amd64/geosquare_v2.duckdb_extension").resolve()
DATABASE = b"signed registry bytes"
KEY_ID = "test-key"
PUBLIC_KEY = "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw="
SIGNATURE = "bKxHstGZfYUsB3RoG8QQsIvO4VozXBSjgI/3rWpvdRkUbo60wkg348q+mstRyGsaghZYtMlFAw5dwyd1JcSLAw=="


def raises(con, sql, params):
    try:
        con.execute(sql, params).fetchall()
    except duckdb.Error:
        return
    raise AssertionError("expected registry verification to fail")


def main():
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT}'")
    with tempfile.TemporaryDirectory(prefix="gs_registry_") as tmp:
        root = Path(tmp)
        database = root / "registry.db"
        signature = root / "registry.db.sig"
        trust = root / "registry-trust.json"
        database.write_bytes(DATABASE)
        signature.write_text(
            json.dumps(
                {
                    "algorithm": "Ed25519",
                    "key_id": KEY_ID,
                    "value": SIGNATURE,
                    "sha256": hashlib.sha256(DATABASE).hexdigest(),
                }
            )
        )
        trust.write_text(json.dumps({KEY_ID: PUBLIC_KEY}))

        verified = con.execute(
            "SELECT verify_registry(?, ?, ?)", [str(database), str(signature), str(trust)]
        ).fetchone()[0]
        assert verified is True

        database.write_bytes(b"tampered registry bytes")
        raises(con, "SELECT verify_registry(?, ?, ?)", [str(database), str(signature), str(trust)])
        database.write_bytes(DATABASE)

        trust.write_text("{}")
        raises(con, "SELECT verify_registry(?, ?, ?)", [str(database), str(signature), str(trust)])

    print("registry verification: OK")


if __name__ == "__main__":
    main()
