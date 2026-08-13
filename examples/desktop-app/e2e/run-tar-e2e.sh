#!/usr/bin/env bash
# The cache-backed tar-layer proof: two real transitions of one real installation.
#
#   ./e2e/build-three-versions.sh /private/tmp/delta-tar-e2e   # once
#   ./e2e/run-tar-e2e.sh          /private/tmp/delta-tar-e2e
#
# One app directory, one cache directory, three launches. The installation moves
# 1.0.0 -> 1.0.1 -> 1.0.2 in place, and the cache has to carry the base from one
# transition to the next, which is the thing under test.
#
# What separates this from the two-version harness: **transition 1 must select
# Full and transition 2 must select TarDelta.** Asserting the installed bytes
# alone cannot tell those apart -- both produce the published artifact -- and an
# updater that silently downloaded everything would pass a hash-only check
# twice. That is not hypothetical; it is what the first real E2E run did
# (docs/DECISIONS.md #22).
#
# Note the output directory must not be under /tmp. On macOS /tmp is a symlink
# to /private/tmp, and Tauri's updater refuses a current_exe() containing a
# symlink.

set -uo pipefail

OUT="${1:-/private/tmp/delta-tar-e2e}"
APP_NAME="DeltaUpdaterExample.app"
MAIN="Contents/MacOS/delta-updater-example"
SCRATCH="$OUT/run"
CACHE="$SCRATCH/cache"
APP="$SCRATCH/app/$APP_NAME"
REPORT="$OUT/e2e-report.json"
FAIL=0

case "$OUT" in /tmp/*) echo "FATAL: use /private/tmp, not /tmp (symlink)" >&2; exit 1;; esac
[ -d "$OUT/v1.0.2" ] || { echo "FATAL: run build-three-versions.sh first" >&2; exit 1; }

H_100="$(shasum -a 256 "$OUT/v1.0.0/$APP_NAME/$MAIN" | awk '{print $1}')"
H_101="$(shasum -a 256 "$OUT/v1.0.1/$APP_NAME/$MAIN" | awk '{print $1}')"
H_102="$(shasum -a 256 "$OUT/v1.0.2/$APP_NAME/$MAIN" | awk '{print $1}')"

# Restated at the point of use, so this script cannot pass vacuously even if run
# against a mis-built set.
if [ "$H_100" = "$H_101" ] || [ "$H_101" = "$H_102" ] || [ "$H_100" = "$H_102" ]; then
  echo "FATAL: the three versions do not have distinct main binaries" >&2
  exit 1
fi

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

# The artifact server, logging every request so a test can assert on what was
# NOT fetched. "It installed the right bytes" is equally true of a full
# download; "it never asked for the full artifact" is not.
start_server() {
  rm -f "$SCRATCH/.port" "$SCRATCH/requests.log"
  # Deliberately NOT `PORT=$(start_server)`. Command substitution reads the
  # function's stdout until every writer closes it, and a backgrounded child
  # inherits that pipe -- so the substitution blocks forever on a server that by
  # design never exits. That is why an earlier harness hung on its first real
  # run. The port goes to a file.
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
}

APP_PID=""
CTL=""
launch() { # $1 = manifest file name served by the artifact server
  rm -f "$SCRATCH/.ctl"
  # Ad-hoc signing and clearing quarantine is the whole Gatekeeper workaround.
  # Re-applied after every install, because the bundle on disk is new.
  codesign --force --deep --sign - "$APP" >/dev/null 2>&1
  xattr -cr "$APP" 2>/dev/null

  DELTA_E2E_CONTROL_PORT_FILE="$SCRATCH/.ctl" \
  DELTA_E2E_MANIFEST_URL="http://127.0.0.1:$PORT/$1" \
  DELTA_E2E_CACHE_DIR="$CACHE" \
  "$APP/$MAIN" >>"$SCRATCH/app.log" 2>&1 &
  APP_PID=$!
  for _ in $(seq 1 80); do [ -s "$SCRATCH/.ctl" ] && break; sleep 0.25; done
  CTL="$(cat "$SCRATCH/.ctl" 2>/dev/null)"
  if [ -z "$CTL" ]; then
    echo "FATAL: the app never opened its control surface (see $SCRATCH/app.log)" >&2
    kill "$APP_PID" "$SERVER_PID" 2>/dev/null
    exit 1
  fi
}

stop_app() {
  [ -n "$APP_PID" ] && kill "$APP_PID" 2>/dev/null
  wait "$APP_PID" 2>/dev/null
  APP_PID=""
}

ask() { curl -s --max-time 300 "http://127.0.0.1:$CTL/$1"; }

installed_hash() { shasum -a 256 "$APP/$MAIN" 2>/dev/null | awk '{print $1}'; }

cleanup() { kill "$APP_PID" "$SERVER_PID" 2>/dev/null; }
trap cleanup EXIT INT TERM

# ---- setup ---------------------------------------------------------------

rm -rf "$SCRATCH"; mkdir -p "$SCRATCH/app" "$CACHE"
cp -R "$OUT/v1.0.0/$APP_NAME" "$SCRATCH/app/"

start_server

# Point the two published manifests at the port that was actually bound. Only
# URLs are rewritten -- every digest, size and signature is the release tool's.
for to in 1.0.1 1.0.2; do
  python3 - "$OUT/manifest-$to.json" "$OUT/manifest-$to.served.json" "$PORT" "$to" <<'PY'
import json, sys
src, dst, port, to = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
m = json.load(open(src))
plat = next(iter(m["platforms"]))
base = f"http://127.0.0.1:{port}"
m["platforms"][plat]["url"] = f"{base}/v{to}/DeltaUpdaterExample.app.tar.gz"
entry = m["delta"]["platforms"][plat]
for frm, patch in entry["patches"].items():
    patch["patch_url"] = f"{base}/patch-{frm}-{to}.zst"
for frm, patch in entry["tar_layer"]["patches"].items():
    patch["patch_url"] = f"{base}/tar-{frm}-{to}.zst"
json.dump(m, open(dst, "w"), indent=2)
PY
done

echo "three distinct main binaries:"
echo "   1.0.0  $H_100"
echo "   1.0.1  $H_101"
echo "   1.0.2  $H_102"
echo

# ---- transition 1: empty cache must select Full --------------------------

echo "== transition 1: 1.0.0 -> 1.0.1, cache EMPTY =="
launch "manifest-1.0.1.served.json"
check "running version" "1.0.0" "$(ask version)"
check "installed binary is 1.0.0" "$H_100" "$(installed_hash)"
check "cache starts empty" "active=none pending=none bytes=0" "$(ask cache)"

T1_START=$(python3 -c 'import time;print(time.time())')
OUTCOME1="$(ask trigger)"
T1_END=$(python3 -c 'import time;print(time.time())')

# The load-bearing assertion. No valid base exists, so the tar path must not be
# taken -- and neither may the direct path, which has no base artifact either.
contains "outcome is a FULL download" "installed-from-full-download" "$OUTCOME1"
not_contains "outcome is not a delta" "installed-from-delta" "$OUTCOME1"
not_contains "outcome is not a tar delta" "installed-from-tar-delta" "$OUTCOME1"

CACHE1="$(ask cache)"
contains "1.0.1 staged as PENDING" "pending=1.0.1@" "$CACHE1"
contains "nothing promoted yet" "active=none" "$CACHE1"

stop_app
check "installed binary is now 1.0.1" "$H_101" "$(installed_hash)"

echo "   -- relaunch as 1.0.1, which is what licenses the promotion --"
launch "manifest-1.0.1.served.json"
check "running version" "1.0.1" "$(ask version)"
CACHE1B="$(ask cache)"
contains "1.0.1 promoted to ACTIVE" "active=1.0.1@" "$CACHE1B"
contains "PENDING cleared" "pending=none" "$CACHE1B"
stop_app
echo

# ---- transition 2: valid cache must select TarDelta ----------------------

echo "== transition 2: 1.0.1 -> 1.0.2, cache ACTIVE(1.0.1) =="
: > "$SCRATCH/requests.log"
launch "manifest-1.0.2.served.json"
check "running version" "1.0.1" "$(ask version)"

T2_START=$(python3 -c 'import time;print(time.time())')
OUTCOME2="$(ask trigger)"
T2_END=$(python3 -c 'import time;print(time.time())')

# The claim this whole branch exists to make.
contains "outcome is a TAR DELTA" "installed-from-tar-delta" "$OUTCOME2"
not_contains "outcome is not a full download" "installed-from-full-download" "$OUTCOME2"

REQUESTS="$(cat "$SCRATCH/requests.log")"
contains "tar patch was downloaded" "/tar-1.0.1-1.0.2.zst" "$REQUESTS"
not_contains "full artifact was NOT downloaded" "/v1.0.2/DeltaUpdaterExample.app.tar.gz" "$REQUESTS"
not_contains "direct patch was NOT downloaded" "/patch-1.0.1-1.0.2.zst" "$REQUESTS"

CACHE2="$(ask cache)"
contains "1.0.2 staged as PENDING" "pending=1.0.2@" "$CACHE2"
contains "1.0.1 still ACTIVE" "active=1.0.1@" "$CACHE2"

stop_app
check "installed binary is now 1.0.2" "$H_102" "$(installed_hash)"

echo "   -- relaunch as 1.0.2 --"
launch "manifest-1.0.2.served.json"
check "running version" "1.0.2" "$(ask version)"
CACHE2B="$(ask cache)"
contains "1.0.2 promoted to ACTIVE" "active=1.0.2@" "$CACHE2B"
contains "PENDING cleared" "pending=none" "$CACHE2B"
stop_app
echo

# ---- measurements --------------------------------------------------------

python3 - "$REPORT" "$OUT" "$SCRATCH" "$CACHE" "$H_100" "$H_101" "$H_102" \
         "$OUTCOME1" "$OUTCOME2" "$T1_START" "$T1_END" "$T2_START" "$T2_END" <<'PY'
import json, os, subprocess, sys
(report, out, scratch, cache, h100, h101, h102,
 outcome1, outcome2, t1s, t1e, t2s, t2e) = sys.argv[1:14]

def du(path):
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except OSError:
                pass
    return total

versions = json.load(open(f"{out}/versions.json"))
json.dump({
    "main_binary_sha256": {"1.0.0": h100, "1.0.1": h101, "1.0.2": h102},
    "transition_1": {
        "from": "1.0.0", "to": "1.0.1",
        "cache_state_before": "EMPTY",
        "outcome": outcome1,
        "seconds": round(float(t1e) - float(t1s), 3),
    },
    "transition_2": {
        "from": "1.0.1", "to": "1.0.2",
        "cache_state_before": "ACTIVE(1.0.1)",
        "outcome": outcome2,
        "seconds": round(float(t2e) - float(t2s), 3),
    },
    "cache_bytes_on_disk": du(cache),
    "build": versions,
}, open(report, "w"), indent=2)
print(f"report written to {report}")
PY

echo
if [ "$FAIL" -eq 0 ]; then
  echo "ALL ASSERTIONS PASSED"
  echo "   transition 1: $OUTCOME1"
  echo "   transition 2: $OUTCOME2"
else
  echo "$FAIL ASSERTION(S) FAILED"
fi
[ "$FAIL" -eq 0 ]
