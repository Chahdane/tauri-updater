#!/usr/bin/env bash
# The real Windows proof: two transitions of one real NSIS installation.
#
#   ./e2e/build-three-versions-windows.sh /c/delta-nsis-e2e   # once
#   ./e2e/run-nsis-e2e.sh                /c/delta-nsis-e2e
#
# One installation, one cache directory, three launches. The installation moves
# 1.0.0 -> 1.0.1 -> 1.0.2 in place, through the real NSIS installer Tauri
# invokes, and the cache has to carry the base from one transition to the next.
#
# **Transition 1 must select DirectDelta from the installer seeded at install
# time (docs/DECISIONS.md #41), transition 2 DirectDelta from the cache, and a
# corrupted cache with no seed must degrade to Full.**
# Asserting the installed bytes alone cannot tell those apart -- both produce
# the published installer -- and an updater that silently downloaded everything
# would pass a hash-only check twice. That is not hypothetical; it is what the
# first real macOS E2E run did (docs/DECISIONS.md #22), and on Windows it is
# also what the shipping client did until the cache learned to hold an opaque
# artifact (#36).
#
# ## Why the outcome is read from a file
#
# `tauri_plugin_updater::Update::install` on Windows calls `ShellExecuteW` and
# then `std::process::exit(0)`. The process is gone before the update returns,
# so `GET /trigger` never responds and no harness can read the `Outcome`. The
# plugin writes the chosen path to `DELTA_E2E_INSTALL_JOURNAL` immediately
# before that handoff, which is the last moment at which it can be observed.
# The server's request log is the independent second witness.

set -uo pipefail

OUT="${1:-/c/delta-nsis-e2e}"
PRODUCT="DeltaUpdaterExample"
SCRATCH="$OUT/run"
CACHE="$SCRATCH/cache"
JOURNAL="$SCRATCH/install-journal"
REPORT="$OUT/e2e-report.json"
FAIL=0
SERVER_PID=""
PORT=""

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) ;;
  *) echo "FATAL: this harness drives a real NSIS install and needs Windows" >&2; exit 1 ;;
esac
[ -d "$OUT/v1.0.2" ] || { echo "FATAL: run build-three-versions-windows.sh first" >&2; exit 1; }

INSTALL_DIR=""
APP_EXE=""

# Where an NSIS `currentUser` install puts things, and what it calls the binary,
# are both *configuration* rather than facts: `installMode` decides the root and
# `mainBinaryName` decides the name. Discovered rather than assumed, and on
# failure the candidate roots are listed -- a harness that guessed would report
# "nothing was installed" for what is really a rename, and say nothing useful
# about where to look instead.
find_app_exe() {
  shopt -s nullglob
  local roots=(
    "$(cygpath -u "$LOCALAPPDATA")/$PRODUCT"
    "$(cygpath -u "$LOCALAPPDATA")/Programs/$PRODUCT"
    "$(cygpath -u "${PROGRAMFILES:-/c/Program Files}")/$PRODUCT"
  )
  local root f
  for root in "${roots[@]}"; do
    [ -d "$root" ] || continue
    local candidates=()
    for f in "$root"/*.exe; do
      case "$(basename "$f")" in
        uninstall.exe|Uninstall.exe|unins*.exe) continue ;;
      esac
      candidates+=("$f")
    done
    if [ "${#candidates[@]}" -eq 1 ]; then
      INSTALL_DIR="$root"
      APP_EXE="${candidates[0]}"
      return 0
    fi
    if [ "${#candidates[@]}" -gt 1 ]; then
      echo "FATAL: $root holds ${#candidates[@]} executables; cannot tell which is the app" >&2
      ls -la "$root" >&2
      return 1
    fi
  done
  echo "FATAL: no installed executable under any of:" >&2
  for root in "${roots[@]}"; do
    echo "         $root" >&2
    [ -d "$root" ] && ls -la "$root" >&2
  done
  return 1
}

H_100="$(sha256sum "$OUT/v1.0.0/$PRODUCT.exe" | awk '{print $1}')"
H_101="$(sha256sum "$OUT/v1.0.1/$PRODUCT.exe" | awk '{print $1}')"
H_102="$(sha256sum "$OUT/v1.0.2/$PRODUCT.exe" | awk '{print $1}')"

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

ps_run() { powershell -NoProfile -NonInteractive -Command "$1"; }

# The process name is the installed executable's stem, and the installer
# relaunches the app itself with `/R`. A process still holding the exe makes the
# next install fail for a reason that has nothing to do with the updater, so
# every transition ends by making sure none is left.
proc_name() {
  if [ -n "$APP_EXE" ]; then basename "$APP_EXE" .exe; else echo "$PRODUCT"; fi
}

kill_app() {
  local name
  name="$(proc_name)"
  ps_run "Get-Process -Name '$name' -ErrorAction SilentlyContinue | Stop-Process -Force" \
    >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do
    if ps_run "if (Get-Process -Name '$name' -ErrorAction SilentlyContinue) { exit 1 } else { exit 0 }" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
}

run_installer() { # $1 = path to a -setup.exe
  local win_path
  win_path="$(cygpath -w "$1")"
  ps_run "Start-Process -FilePath '$win_path' -ArgumentList '/S' -Wait" >/dev/null
}

uninstall_if_present() {
  local root
  for root in \
    "$(cygpath -u "$LOCALAPPDATA")/$PRODUCT" \
    "$(cygpath -u "$LOCALAPPDATA")/Programs/$PRODUCT"; do
    if [ -f "$root/uninstall.exe" ]; then
      ps_run "Start-Process -FilePath '$(cygpath -w "$root/uninstall.exe")' -ArgumentList '/S' -Wait" \
        >/dev/null 2>&1 || true
      sleep 2
    fi
    rm -rf "$root" 2>/dev/null || true
  done
  INSTALL_DIR=""
  APP_EXE=""
}

# The artifact server, logging every request so a test can assert on what was
# NOT fetched. "It installed the right bytes" is equally true of a full
# download; "it never asked for the full artifact" is not.
start_server() {
  rm -f "$SCRATCH/.port" "$SCRATCH/requests.log"
  # Deliberately NOT `PORT=$(start_server)`. Command substitution reads the
  # function's stdout until every writer closes it, and a backgrounded child
  # inherits that pipe -- so the substitution blocks forever on a server that by
  # design never exits. The port goes to a file.
  python - "$OUT" "$SCRATCH" >"$SCRATCH/server.log" 2>&1 <<'PYSERVER' &
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
PYSERVER
  SERVER_PID=$!
  for _ in $(seq 1 40); do [ -s "$SCRATCH/.port" ] && break; sleep 0.25; done
  [ -s "$SCRATCH/.port" ] || { echo "FATAL: artifact server never bound a port" >&2; exit 1; }
  PORT="$(cat "$SCRATCH/.port")"
}

APP_PID=""
CTL=""
launch() { # $1 = manifest file name served by the artifact server
  rm -f "$SCRATCH/.ctl"
  DELTA_E2E_CONTROL_PORT_FILE="$(cygpath -w "$SCRATCH/.ctl")" \
  DELTA_E2E_MANIFEST_URL="http://127.0.0.1:$PORT/$1" \
  DELTA_E2E_CACHE_DIR="$(cygpath -w "$CACHE")" \
  DELTA_E2E_INSTALL_JOURNAL="$(cygpath -w "$JOURNAL")" \
  "$APP_EXE" >>"$SCRATCH/app.log" 2>&1 &
  APP_PID=$!
  # A cold runner's first launch initialises WebView2, which is slow and only
  # slow once. Generous rather than tight: a timeout here reads as "the harness
  # is broken" and costs a whole CI round to find out otherwise.
  for _ in $(seq 1 360); do [ -s "$SCRATCH/.ctl" ] && break; sleep 0.5; done
  CTL="$(cat "$SCRATCH/.ctl" 2>/dev/null)"
  if [ -z "$CTL" ]; then
    echo "FATAL: the app never opened its control surface" >&2
    echo "       app.log:" >&2
    tail -40 "$SCRATCH/app.log" >&2 2>/dev/null || echo "       (empty)" >&2
    kill_app
    kill "$SERVER_PID" 2>/dev/null
    exit 1
  fi
}

ask() { curl -s --max-time 120 "http://127.0.0.1:$CTL/$1"; }

# The one request that is expected never to answer.
trigger() {
  rm -f "$JOURNAL"
  curl -s --max-time 600 "http://127.0.0.1:$CTL/trigger" >/dev/null 2>&1 || true
}

installed_hash() { sha256sum "$APP_EXE" 2>/dev/null | awk '{print $1}'; }

# The installer runs asynchronously once ShellExecuteW returns, so the install
# is complete when the bytes on disk are the ones being installed -- not when
# any process we can see has exited.
wait_for_installed() { # $1 = expected sha256
  for _ in $(seq 1 240); do
    [ "$(installed_hash)" = "$1" ] && return 0
    sleep 1
  done
  return 1
}

journal() { cat "$JOURNAL" 2>/dev/null || echo "<no journal written>"; }

cleanup() {
  kill_app
  kill "$SERVER_PID" 2>/dev/null
}
trap cleanup EXIT INT TERM

# ---- setup ---------------------------------------------------------------

rm -rf "$SCRATCH"; mkdir -p "$SCRATCH" "$CACHE"
echo "==> removing any previous installation"
kill_app
uninstall_if_present

echo "==> installing 1.0.0 from its own NSIS installer"
run_installer "$OUT/v1.0.0/$PRODUCT-setup.exe"
kill_app
find_app_exe || exit 1
echo "    installed as $APP_EXE"
check "installed executable is the 1.0.0 build" "$H_100" "$(installed_hash)"

start_server

# Point the two published manifests at the port that was actually bound. Only
# URLs are rewritten -- every digest, size and signature is the release tool's.
for to in 1.0.1 1.0.2; do
  python - "$OUT/manifest-$to.json" "$OUT/manifest-$to.served.json" "$PORT" "$to" "$PRODUCT" <<'PY'
import json, sys
src, dst, port, to, product = sys.argv[1:6]
m = json.load(open(src))
plat = next(iter(m["platforms"]))
base = f"http://127.0.0.1:{port}"
m["platforms"][plat]["url"] = f"{base}/v{to}/{product}-setup.exe"
entry = m["delta"]["platforms"][plat]
for frm, patch in entry["patches"].items():
    patch["patch_url"] = f"{base}/patch-{frm}-{to}.zst"
json.dump(m, open(dst, "w"), indent=2)
PY
done

echo
echo "three distinct main binaries:"
echo "   1.0.0  $H_100"
echo "   1.0.1  $H_101"
echo "   1.0.2  $H_102"
echo

seed_hash() {
  local seed
  seed="$(dirname "$APP_EXE")/delta-seed/installer.exe"
  [ -f "$seed" ] && sha256sum "$seed" | awk '{print $1}' || echo "no-seed"
}
SETUP_100="$(sha256sum "$OUT/v1.0.0/$PRODUCT-setup.exe" | awk '{print $1}')"
SETUP_101="$(sha256sum "$OUT/v1.0.1/$PRODUCT-setup.exe" | awk '{print $1}')"

# ---- transition 1: empty cache, seeded installer, must select DirectDelta ---

echo "== transition 1: 1.0.0 -> 1.0.1, cache EMPTY, installer seeded =="
check "the installer hook kept the 1.0.0 installer" "$SETUP_100" "$(seed_hash)"
: > "$SCRATCH/requests.log"
launch "manifest-1.0.1.served.json"
check "running version" "1.0.0" "$(ask version)"
check "installed executable is the 1.0.0 build" "$H_100" "$(installed_hash)"
# Non-vacuity: if the installation already were 1.0.1 or 1.0.2, every
# "installed executable is now ..." check downstream would pass without an
# install having happened.
not_contains "starting binary is not 1.0.1" "$H_101" "$(installed_hash)"
not_contains "starting binary is not 1.0.2" "$H_102" "$(installed_hash)"
check "cache starts empty" "active=none pending=none bytes=0" "$(ask cache)"

T1_START=$(python -c 'import time;print(time.time())')
trigger
OUTCOME1="$(journal)"
wait_for_installed "$H_101" || true
T1_END=$(python -c 'import time;print(time.time())')

# The load-bearing assertion. The cache is empty, so the only base that can
# make this a delta is the installer the hook kept.
check "selected path is a DIRECT DELTA from the seed" "delta" "$OUTCOME1"
REQUESTS1="$(cat "$SCRATCH/requests.log")"
contains "the patch was downloaded" "/patch-1.0.0-1.0.1.zst" "$REQUESTS1"
not_contains "the full installer was NOT downloaded" "/v1.0.1/$PRODUCT-setup.exe" "$REQUESTS1"

kill_app
check "installed executable is now 1.0.1" "$H_101" "$(installed_hash)"
# The updater ran the 1.0.1 installer silently; the hook must have replaced the
# seed, so it keeps describing the installed version.
check "the seed is now the 1.0.1 installer" "$SETUP_101" "$(seed_hash)"

echo "   -- relaunch as 1.0.1, which is what licenses the promotion --"
launch "manifest-1.0.1.served.json"
check "running version" "1.0.1" "$(ask version)"
CACHE1="$(ask cache)"
contains "1.0.1 promoted to ACTIVE" "active=1.0.1@" "$CACHE1"
contains "PENDING cleared" "pending=none" "$CACHE1"
kill_app
echo

# ---- transition 2: valid cache must select DirectDelta -------------------

echo "== transition 2: 1.0.1 -> 1.0.2, cache ACTIVE(1.0.1) =="
: > "$SCRATCH/requests.log"
launch "manifest-1.0.2.served.json"
check "running version" "1.0.1" "$(ask version)"
contains "1.0.1 is the cached base" "active=1.0.1@" "$(ask cache)"

T2_START=$(python -c 'import time;print(time.time())')
trigger
OUTCOME2="$(journal)"
wait_for_installed "$H_102" || true
T2_END=$(python -c 'import time;print(time.time())')

# The claim this whole branch exists to make.
check "selected path is a DIRECT DELTA" "delta" "$OUTCOME2"
REQUESTS2="$(cat "$SCRATCH/requests.log")"
contains "the patch was downloaded" "/patch-1.0.1-1.0.2.zst" "$REQUESTS2"
not_contains "the full installer was NOT downloaded" "/v1.0.2/$PRODUCT-setup.exe" "$REQUESTS2"

kill_app
check "installed executable is now 1.0.2" "$H_102" "$(installed_hash)"

echo "   -- relaunch as 1.0.2 --"
launch "manifest-1.0.2.served.json"
check "running version" "1.0.2" "$(ask version)"
CACHE2="$(ask cache)"
contains "1.0.2 promoted to ACTIVE" "active=1.0.2@" "$CACHE2"
contains "PENDING cleared" "pending=none" "$CACHE2"
kill_app
echo

# ---- degradation, against the real cache on disk -------------------------
#
# The fallback matrix is covered exhaustively by
# crates/plugin/tests/direct_delta_flow.rs, which can assert that the direct
# path was *attempted* before it fell back -- evidence this harness cannot
# produce, since the app reports a selection rather than a plan. What only a
# real run can show is that a damaged cache degrades a **real Windows install**
# rather than breaking it, so that is what this section does.

echo "== degradation: a corrupted cache blob must fall back to Full =="
echo "   -- reinstalling 1.0.1 and reseeding the cache through a real update --"
kill_app
uninstall_if_present
rm -rf "$CACHE"; mkdir -p "$CACHE"
run_installer "$OUT/v1.0.0/$PRODUCT-setup.exe"
kill_app
find_app_exe || exit 1
launch "manifest-1.0.1.served.json"
trigger
wait_for_installed "$H_101" || true
kill_app
launch "manifest-1.0.2.served.json"
contains "1.0.1 is ACTIVE again" "active=1.0.1@" "$(ask cache)"
kill_app

# Same length, different bytes: only the digest can catch it.
BLOB="$(ls "$CACHE"/blobs/* 2>/dev/null | head -1)"
if [ -z "$BLOB" ]; then
  echo "     FAIL no cached blob to corrupt"
  FAIL=$((FAIL+1))
else
  python - "$BLOB" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
b[len(b) // 2] ^= 0xFF
open(p, "wb").write(bytes(b))
PY
  echo "     (corrupted $(basename "$BLOB"))"

  # Without this the seed would supply the base and hide the cache failure.
  rm -rf "$(dirname "$APP_EXE")/delta-seed"
  : > "$SCRATCH/requests.log"
  launch "manifest-1.0.2.served.json"
  trigger
  OUTCOME3="$(journal)"
  wait_for_installed "$H_102" || true
  check "a corrupt cache degrades to FULL" "full" "$OUTCOME3"
  contains "the full installer was downloaded" "/v1.0.2/$PRODUCT-setup.exe" "$(cat "$SCRATCH/requests.log")"
  kill_app
  check "and 1.0.2 still installed correctly" "$H_102" "$(installed_hash)"
fi
echo

# ---- measurements --------------------------------------------------------

python - "$REPORT" "$OUT" "$CACHE" "$H_100" "$H_101" "$H_102" \
         "$OUTCOME1" "$OUTCOME2" "$T1_START" "$T1_END" "$T2_START" "$T2_END" \
         "${OUTCOME3:-not-run}" <<'PY'
import json, os, sys
(report, out, cache, h100, h101, h102,
 outcome1, outcome2, t1s, t1e, t2s, t2e, outcome3) = sys.argv[1:14]

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
    "platform": versions["platform"],
    "installer": "nsis",
    "main_binary_sha256": {"1.0.0": h100, "1.0.1": h101, "1.0.2": h102},
    "transition_1": {
        "from": "1.0.0", "to": "1.0.1",
        "cache_state_before": "EMPTY, installer seeded",
        "selected_path": outcome1,
        "seconds": round(float(t1e) - float(t1s), 3),
    },
    "transition_2": {
        "from": "1.0.1", "to": "1.0.2",
        "cache_state_before": "ACTIVE(1.0.1)",
        "selected_path": outcome2,
        "seconds": round(float(t2e) - float(t2s), 3),
    },
    "degradation_corrupt_cache_blob_no_seed": {"selected_path": outcome3},
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
