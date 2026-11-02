#!/usr/bin/env python3
"""Regenerates src/auth/cookiedb-fixture.sqlite: fake cookies in Chromium's schema."""
import os, sqlite3

path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src", "auth", "cookiedb-fixture.sqlite")
if os.path.exists(path):
    os.remove(path)
db = sqlite3.connect(path)
db.executescript("""
PRAGMA page_size=4096;
CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR);
INSERT INTO meta VALUES ('version', '24'), ('last_compatible_version', '24');
CREATE TABLE cookies(creation_utc INTEGER NOT NULL, host_key TEXT NOT NULL, top_frame_site_key TEXT NOT NULL,
  name TEXT NOT NULL, value TEXT NOT NULL, encrypted_value BLOB NOT NULL, path TEXT NOT NULL,
  expires_utc INTEGER NOT NULL, is_secure INTEGER NOT NULL, is_httponly INTEGER NOT NULL,
  last_access_utc INTEGER NOT NULL, has_expires INTEGER NOT NULL DEFAULT 1,
  UNIQUE (host_key, top_frame_site_key, name, path));
""")
row = "INSERT INTO cookies VALUES (1, ?, '', ?, '', ?, '/', ?, ?, ?, 1, 1)"
db.execute(row, (".youtube.com", "SAPISID", b"v10fake", 13_500_000_000_000_000, 1, 0))
db.execute(row, (".youtube.com", "LONG", bytes(i % 251 for i in range(10_000)), 0, 0, 1))
for i in range(299):
    db.execute(row, (".google.com", f"filler{i}", b"v10" + b"x" * 60, 0, 1, 1))
db.commit()
db.execute("VACUUM")
db.close()
print(os.path.getsize(path), "bytes")
