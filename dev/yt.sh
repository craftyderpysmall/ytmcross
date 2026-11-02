#!/bin/sh
# Authenticated InnerTube probe: yt.sh <endpoint> '<json body without context>' <outfile>
data="${XDG_DATA_HOME:-$HOME/.local/share}/ytmcross"
exec python3 -I - "$data/cookies.txt" "$@" <<'PY'
import datetime, hashlib, json, subprocess, sys, time
jar, endpoint, body, out = sys.argv[1:5]
cookies = {}
for line in open(jar):
    line = line.removeprefix("#HttpOnly_").rstrip("\n")
    fields = line.split("\t")
    if len(fields) >= 7 and not line.startswith("#"):
        cookies[fields[5]] = fields[6]
ts = int(time.time())
sign = lambda name: hashlib.sha1(f"{ts} {cookies[name]} https://music.youtube.com".encode()).hexdigest()
auth = (f"SAPISIDHASH {ts}_{sign('SAPISID')} SAPISID1PHASH {ts}_{sign('__Secure-1PAPISID')} "
        f"SAPISID3PHASH {ts}_{sign('__Secure-3PAPISID')}")
version = "1." + datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d") + ".01.00"
payload = json.loads(body)
payload["context"] = {"client": {"clientName": "WEB_REMIX", "clientVersion": version, "hl": "en"}}
config = "".join(f'header = "{h}"\n' for h in [
    "Cookie: " + "; ".join(f"{k}={v}" for k, v in cookies.items()), f"Authorization: {auth}",
    "Origin: https://music.youtube.com", "X-Origin: https://music.youtube.com", "X-Goog-AuthUser: 0",
    "Content-Type: application/json"])
subprocess.run(["curl", "-s", "-K", "-", "-A", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
                "--data-binary", json.dumps(payload), "-o", out,
                f"https://music.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"], input=config.encode(), check=True)
print(out)
PY
