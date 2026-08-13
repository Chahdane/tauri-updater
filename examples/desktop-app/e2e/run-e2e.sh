#!/usr/bin/env bash
# The end-to-end proof: a running Tauri app accepts a served update.
#
#   ./e2e/build-versions.sh /private/tmp/delta-e2e-build   # once
#   ./e2e/run-e2e.sh        /private/tmp/delta-e2e-build
#
# Each scenario gets a fresh copy of v1.0.0, is served a manifest over plain
# loopback HTTP, is launched, is triggered through the test-only control
# surface, and is then judged by **what is on disk**: the BLAKE3 of the
# installed main binary, by name, by exact hash. Not "install() returned Ok".
#
# Note the output directory must not be under /tmp. On macOS /tmp is a symlink
# to /private/tmp, and Tauri's updater refuses a current_exe() containing a
# symlink ("StartingBinary found current_exe() that contains a symlink on a
# non-allowed platform"). Use the real path.

set -uo pipefail

OUT="${1:-/private/tmp/delta-e2e-build}"
APP_NAME="DeltaUpdaterExample.app"
MAIN="Contents/MacOS/delta-updater-example"
SCRATCH="$OUT/run"
PASS=0; FAIL=0

case "$OUT" in /tmp/*) echo "FATAL: use /private/tmp, not /tmp (symlink)" >&2; exit 1;; esac
[ -d "$OUT/v1.0.1" ] || { echo "FATAL: run build-versions.sh first" >&2; exit 1; }

EXPECTED_NEW="$(shasum -a 256 "$OUT/v1.0.1/$APP_NAME/$MAIN" | awk '{print $1}')"
EXPECTED_OLD="$(shasum -a 256 "$OUT/v1.0.0/$APP_NAME/$MAIN" | awk '{print $1}')"

# The fixture-difference discipline, restated at the point of use so this script
# cannot pass vacuously even if run against a mis-built pair.
if [ "$EXPECTED_NEW" = "$EXPECTED_OLD" ]; then
  echo "FATAL: v1.0.0 and v1.0.1 have identical main binaries — every assertion below would be vacuous" >&2
  exit 1
fi

# Starts the artifact server. Sets SERVER_PID and writes the chosen port to
# "$1/.port"; the caller reads it from there.
#
# Deliberately NOT `port=$(start_server ...)`. Command substitution reads the
# function's stdout until every writer closes it, and a backgrounded child
# inherits that pipe — so the substitution blocks forever waiting on a server
# that by design never exits. That is not hypothetical: it is why this harness
# hung on its first real run, and why the end-to-end claim stayed open this long.
# The server's output goes to a file and nothing here writes to stdout.
start_server() { # $1 = doc root
  rm -f "$1/.port"
  python3 - "$1" >"$1/server.log" 2>&1 <<'PY' &
import http.server, socketserver, os, sys
os.chdir(sys.argv[1])
class H(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a): pass
with socketserver.TCPServer(("127.0.0.1", 0), H) as httpd:
    open(os.path.join(sys.argv[1], ".port"), "w").write(str(httpd.server_address[1]))
    httpd.serve_forever()
PY
  SERVER_PID=$!
  for _ in $(seq 1 40); do [ -s "$1/.port" ] && break; sleep 0.25; done
  [ -s "$1/.port" ] || { echo "FATAL: artifact server never bound a port" >&2; return 1; }
}

# Run one scenario. $1 name, $2 mutation (a shell snippet over $ROOT), $3 expected
# main-binary hash after install or the literal word UNCHANGED, $4 substring the
# reported outcome must contain.
#
# $4 exists because the first real run of this harness passed every scenario
# while silently taking the full-download path: the installed bytes were correct,
# so a hash-only check could not tell that the delta path had never run. Judging
# the bytes is necessary and is not sufficient — the path taken is a separate
# claim and needs its own assertion.
scenario() {
  local name="$1" mutate="$2" expect="$3" want_outcome="${4:-}"
  local ROOT="$SCRATCH/$name"
  rm -rf "$ROOT"; mkdir -p "$ROOT"

  cp -R "$OUT/v1.0.0/$APP_NAME" "$ROOT/"
  cp "$OUT/v1.0.0/$APP_NAME.tar.gz" "$ROOT/base.tar.gz"
  mkdir -p "$ROOT/v1.0.1"
  cp "$OUT/v1.0.1/$APP_NAME.tar.gz" "$ROOT/v1.0.1/"
  cp "$OUT/patch.zst" "$ROOT/patch.zst"
  cp "$OUT/manifest.json" "$ROOT/manifest.json"

  # Unsigned bundles will not launch; ad-hoc signing and clearing quarantine is
  # the whole Gatekeeper workaround. Nothing paid, nothing notarized.
  codesign --force --deep --sign - "$ROOT/$APP_NAME" >/dev/null 2>&1
  xattr -cr "$ROOT/$APP_NAME" 2>/dev/null

  local port
  start_server "$ROOT" || { FAIL=$((FAIL+1)); return; }
  port="$(cat "$ROOT/.port")"
  python3 - "$ROOT/manifest.json" "$port" <<'PY'
import json, sys
p, port = sys.argv[1], sys.argv[2]
m = json.load(open(p))
plat = next(iter(m["platforms"]))
m["platforms"][plat]["url"] = f"http://127.0.0.1:{port}/v1.0.1/DeltaUpdaterExample.app.tar.gz"
m["delta"]["platforms"][plat]["patches"]["1.0.0"]["patch_url"] = f"http://127.0.0.1:{port}/patch.zst"
json.dump(m, open(p, "w"), indent=2)
PY

  eval "$mutate"   # scenario-specific damage, applied after the manifest is live

  rm -f "$ROOT/.ctl"
  DELTA_E2E_CONTROL_PORT_FILE="$ROOT/.ctl" \
  DELTA_E2E_MANIFEST_URL="http://127.0.0.1:$port/manifest.json" \
  DELTA_E2E_BASE_ARTIFACT="$ROOT/base.tar.gz" \
  "$ROOT/$APP_NAME/$MAIN" >"$ROOT/app.log" 2>&1 &
  local app_pid=$!
  for _ in $(seq 1 60); do [ -s "$ROOT/.ctl" ] && break; sleep 0.25; done

  local ctl outcome
  ctl="$(cat "$ROOT/.ctl" 2>/dev/null)"
  if [ -z "$ctl" ]; then
    echo "  FAIL $name — app never opened its control surface"; FAIL=$((FAIL+1))
    kill $app_pid $SERVER_PID 2>/dev/null; return
  fi
  outcome="$(curl -s --max-time 180 "http://127.0.0.1:$ctl/trigger")"
  kill $app_pid $SERVER_PID 2>/dev/null
  wait $app_pid 2>/dev/null

  local actual; actual="$(shasum -a 256 "$ROOT/$APP_NAME/$MAIN" 2>/dev/null | awk '{print $1}')"
  local want="$expect"; [ "$expect" = "UNCHANGED" ] && want="(unchanged)"

  # The judgement is the bytes on disk, not the outcome string.
  local ok=1 why=""
  if [ "$expect" = "UNCHANGED" ]; then
    [ "$actual" != "$EXPECTED_NEW" ] || { ok=0; why="binary changed when it must not have"; }
  else
    [ "$actual" = "$expect" ] || { ok=0; why="installed hash mismatch"; }
  fi
  if [ -n "$want_outcome" ] && [[ "$outcome" != *"$want_outcome"* ]]; then
    ok=0; why="outcome did not contain '$want_outcome'"
  fi

  if [ $ok -eq 1 ]; then
    printf "  PASS %-38s %s\n" "$name" "${outcome:0:60}"
    PASS=$((PASS+1))
  else
    printf "  FAIL %-38s (%s)\n       outcome: %s\n       want %s\n       got  %s\n" \
      "$name" "$why" "$outcome" "$want" "$actual"
    FAIL=$((FAIL+1))
  fi
}

echo "expected v1.0.1 main binary: $EXPECTED_NEW"
echo

echo "-- happy path --"
scenario "delta-install"            ":"                                          "$EXPECTED_NEW" "installed-from-delta"

echo "-- fallback: installed bytes must still be the release --"
scenario "corrupt-patch"            'printf junk > "$ROOT/patch.zst"'            "$EXPECTED_NEW" "installed-from-full-download"
scenario "truncated-patch"          'A=$(stat -f%z "$ROOT/patch.zst"); dd if="$ROOT/patch.zst" of="$ROOT/p2" bs=1 count=$((A/2)) 2>/dev/null; mv "$ROOT/p2" "$ROOT/patch.zst"' "$EXPECTED_NEW" "installed-from-full-download"
scenario "missing-patch"            'rm -f "$ROOT/patch.zst"'                    "$EXPECTED_NEW" "installed-from-full-download"
scenario "wrong-base"               'printf "not the base artifact" > "$ROOT/base.tar.gz"' "$EXPECTED_NEW" "installed-from-full-download"

echo "-- refusal: nothing may be installed --"
scenario "tampered-full-download"   'printf "malicious" > "$ROOT/v1.0.1/DeltaUpdaterExample.app.tar.gz"; rm -f "$ROOT/patch.zst"' UNCHANGED "error"
scenario "tampered-both"            'printf junk > "$ROOT/patch.zst"; printf "malicious" > "$ROOT/v1.0.1/DeltaUpdaterExample.app.tar.gz"' UNCHANGED "error"

echo
echo "passed $PASS, failed $FAIL"
[ $FAIL -eq 0 ]
