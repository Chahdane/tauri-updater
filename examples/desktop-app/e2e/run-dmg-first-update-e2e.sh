#!/usr/bin/env bash
# The first macOS update of an app installed from its disk image, cache EMPTY.
#
#   BUILD_DMG=1 ./e2e/build-three-versions.sh /private/tmp/delta-dmg-e2e   # once
#   ./e2e/run-dmg-first-update-e2e.sh           /private/tmp/delta-dmg-e2e
#
# Installs 1.0.0 the way a user does -- mount the DMG, copy the .app out with
# `ditto` (the metadata-preserving copy Finder performs), eject -- and launches
# it UNTOUCHED: no re-signing and no attribute stripping, because either would
# change the bundle the plugin rebuilds its base from. Then one update to 1.0.1
# with nothing cached. docs/DECISIONS.md #37 says the tar path may rebuild its
# base from the installed bundle; this is the real-install test of that claim.
#
# Pass: the update is a TarDelta, the tar patch was fetched and the full
# artifact was not, and the installed binary is exactly 1.0.1. Whatever the
# outcome, dmg-first-update-report.json records which tar header fields the
# installed bundle did and did not preserve, so a Full result explains itself.

set -uo pipefail

OUT="${1:-/private/tmp/delta-dmg-e2e}"
APP_NAME="DeltaUpdaterExample.app"
MAIN="Contents/MacOS/delta-updater-example"
SCRATCH="$OUT/dmg-run"
CACHE="$SCRATCH/cache"
APP="$SCRATCH/Applications/$APP_NAME"
REPORT="$OUT/dmg-first-update-report.json"
DMG="$OUT/v1.0.0/DeltaUpdaterExample.dmg"
FAIL=0

case "$OUT" in /tmp/*) echo "FATAL: use /private/tmp, not /tmp (symlink)" >&2; exit 1;; esac
[ -f "$DMG" ] || { echo "FATAL: run BUILD_DMG=1 build-three-versions.sh first" >&2; exit 1; }

H_101="$(shasum -a 256 "$OUT/v1.0.1/$APP_NAME/$MAIN" | awk '{print $1}')"

check() { # $1 label, $2 expected, $3 actual
  if [ "$2" = "$3" ]; then
    printf "     ok   %-46s %s\n" "$1" "${3:0:44}"
  else
    printf "     FAIL %-46s\n          want %s\n          got  %s\n" "$1" "$2" "$3"
    FAIL=$((FAIL+1))
  fi
}
contains() { # $1 label, $2 needle, $3 haystack
  if [[ "$3" == *"$2"* ]]; then
    printf "     ok   %-46s %s\n" "$1" "${3:0:44}"
  else
    printf "     FAIL %-46s\n          want to contain %s\n          got  %s\n" "$1" "$2" "$3"
    FAIL=$((FAIL+1))
  fi
}
not_contains() { # $1 label, $2 needle, $3 haystack
  if [[ "$3" != *"$2"* ]]; then
    printf "     ok   %-46s\n" "$1"
  else
    printf "     FAIL %-46s\n          must not contain %s\n" "$1" "$2"
    FAIL=$((FAIL+1))
  fi
}

APP_PID=""
SERVER_PID=""
cleanup() {
  kill "$APP_PID" "$SERVER_PID" 2>/dev/null
  hdiutil detach "$SCRATCH/mnt" -quiet 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ---- install from the disk image -----------------------------------------

rm -rf "$SCRATCH"; mkdir -p "$SCRATCH/Applications" "$SCRATCH/mnt" "$CACHE"
hdiutil attach "$DMG" -nobrowse -readonly -mountpoint "$SCRATCH/mnt" -quiet \
  || { echo "FATAL: could not mount $DMG" >&2; exit 1; }
ditto "$SCRATCH/mnt/$APP_NAME" "$APP"
hdiutil detach "$SCRATCH/mnt" -quiet

# Before anything runs: which tar header fields did the install keep?
python3 - "$OUT/v1.0.0/DeltaUpdaterExample.app.tar.gz" "$APP" "$SCRATCH/metadata.json" <<'PY'
import json, os, stat, sys, tarfile
archive, app, out = sys.argv[1:4]
fields = {"mtime": 0, "uid": 0, "gid": 0, "mode": 0, "size": 0, "missing": 0}
examples = {}
entries = 0
with tarfile.open(archive, "r:gz") as tar:
    for m in tar.getmembers():
        entries += 1
        rel = m.name.split("/", 1)[1] if "/" in m.name else ""
        path = os.path.join(app, rel) if rel else app
        try:
            st = os.lstat(path)
        except FileNotFoundError:
            fields["missing"] += 1
            continue
        got = {"mtime": int(st.st_mtime), "uid": st.st_uid, "gid": st.st_gid,
               "mode": stat.S_IMODE(st.st_mode), "size": st.st_size if m.isfile() else 0}
        want = {"mtime": int(m.mtime), "uid": m.uid, "gid": m.gid,
                "mode": m.mode & 0o7777, "size": m.size if m.isfile() else 0}
        for k in got:
            if got[k] != want[k]:
                fields[k] += 1
                examples.setdefault(k, {"entry": m.name, "published": want[k], "installed": got[k]})
json.dump({"entries": entries, "mismatches": fields, "first_mismatch": examples},
          open(out, "w"), indent=2)
print(f"   tar entries {entries}; header fields that differ after install: {fields}")
PY

# ---- serve the 1.0.1 release ---------------------------------------------

rm -f "$SCRATCH/.port" "$SCRATCH/requests.log"
python3 - "$OUT" "$SCRATCH" >"$SCRATCH/server.log" 2>&1 <<'PY' &
import http.server, socketserver, os, sys
root, scratch = sys.argv[1], sys.argv[2]
os.chdir(root)
log = open(os.path.join(scratch, "requests.log"), "a", buffering=1)
class H(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        log.write(self.path + "\n")
        return super().do_GET()
with socketserver.TCPServer(("127.0.0.1", 0), H) as httpd:
    open(os.path.join(scratch, ".port"), "w").write(str(httpd.server_address[1]))
    httpd.serve_forever()
PY
SERVER_PID=$!
for _ in $(seq 1 40); do [ -s "$SCRATCH/.port" ] && break; sleep 0.25; done
[ -s "$SCRATCH/.port" ] || { echo "FATAL: artifact server never bound a port" >&2; exit 1; }
PORT="$(cat "$SCRATCH/.port")"

python3 - "$OUT/manifest-1.0.1.json" "$OUT/manifest-1.0.1.dmg.json" "$PORT" <<'PY'
import json, sys
src, dst, port = sys.argv[1:4]
m = json.load(open(src))
plat = next(iter(m["platforms"]))
base = f"http://127.0.0.1:{port}"
m["platforms"][plat]["url"] = f"{base}/v1.0.1/DeltaUpdaterExample.app.tar.gz"
entry = m["delta"]["platforms"][plat]
for frm, patch in entry["patches"].items():
    patch["patch_url"] = f"{base}/patch-{frm}-1.0.1.zst"
for frm, patch in entry["tar_layer"]["patches"].items():
    patch["patch_url"] = f"{base}/tar-{frm}-1.0.1.zst"
json.dump(m, open(dst, "w"), indent=2)
PY

# ---- the first update ----------------------------------------------------

echo "== 1.0.0 (installed from its DMG) -> 1.0.1, cache EMPTY =="
rm -f "$SCRATCH/.ctl"
DELTA_E2E_CONTROL_PORT_FILE="$SCRATCH/.ctl" \
DELTA_E2E_MANIFEST_URL="http://127.0.0.1:$PORT/manifest-1.0.1.dmg.json" \
DELTA_E2E_CACHE_DIR="$CACHE" \
"$APP/$MAIN" >>"$SCRATCH/app.log" 2>&1 &
APP_PID=$!
for _ in $(seq 1 80); do [ -s "$SCRATCH/.ctl" ] && break; sleep 0.25; done
CTL="$(cat "$SCRATCH/.ctl" 2>/dev/null)"
[ -n "$CTL" ] || { echo "FATAL: the app never opened its control surface" >&2; cat "$SCRATCH/app.log" >&2; exit 1; }
ask() { curl -s --max-time 300 "http://127.0.0.1:$CTL/$1"; }

check "running version" "1.0.0" "$(ask version)"
check "cache starts empty" "active=none pending=none bytes=0" "$(ask cache)"
: > "$SCRATCH/requests.log"
OUTCOME="$(ask trigger)"
REQUESTS="$(cat "$SCRATCH/requests.log")"

contains "outcome is a TAR DELTA from the installed app" "installed-from-tar-delta" "$OUTCOME"
contains "the tar patch was downloaded" "/tar-1.0.0-1.0.1.zst" "$REQUESTS"
not_contains "the full artifact was NOT downloaded" "/v1.0.1/DeltaUpdaterExample.app.tar.gz" "$REQUESTS"

kill "$APP_PID" 2>/dev/null; wait "$APP_PID" 2>/dev/null; APP_PID=""
check "installed binary is now 1.0.1" "$H_101" "$(shasum -a 256 "$APP/$MAIN" | awk '{print $1}')"

python3 - "$REPORT" "$SCRATCH/metadata.json" "$OUTCOME" "$REQUESTS" "$FAIL" <<'PY'
import json, sys
report, meta, outcome, requests, fail = sys.argv[1:6]
json.dump({
    "install": "DMG mounted read-only, .app copied with ditto, launched unmodified",
    "cache_before": "EMPTY",
    "outcome": outcome,
    "requests": [r for r in requests.splitlines() if r],
    "installed_metadata_vs_published_tar": json.load(open(meta)),
    "assertion_failures": int(fail),
}, open(report, "w"), indent=2)
print(f"report written to {report}")
PY

echo
if [ "$FAIL" -eq 0 ]; then
  echo "ALL ASSERTIONS PASSED: the first update after a DMG install was a TarDelta"
else
  echo "$FAIL ASSERTION(S) FAILED (see $REPORT for which metadata the install lost)"
fi
[ "$FAIL" -eq 0 ]
