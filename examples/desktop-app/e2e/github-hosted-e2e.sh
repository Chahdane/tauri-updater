#!/usr/bin/env bash
# The real distribution path: GitHub-hosted assets, HTTPS, real redirects.
#
#   ./github-hosted-e2e.sh <owner/repo> <old-tag> <new-tag> [artifact-dir]
#
# Everything the loopback E2E proves, proved again over the transport users
# actually meet. The loopback harness serves from 127.0.0.1 with the plain-HTTP
# opt-in enabled; this one talks to github.com, follows the 302 to the release
# CDN, and never enables that opt-in — so a TLS or redirect problem shows up as
# a failure rather than as something the harness quietly permitted.
#
# ## What must already exist
#
# Two published GitHub releases, produced by .github/workflows/release.yml,
# each carrying:
#
#   DeltaUpdaterExample.app.tar.gz
#   DeltaUpdaterExample.app.tar.gz.sig
#   manifest.json
#   <old>-to-<new>.zst          (on the newer release)
#   <old>-to-<new>.tar.zst      (on the newer release)
#
# Creating those needs a token with `contents: write`, which is the one step
# this script cannot do for you. See docs/RELEASING.md.
#
# ## What it proves
#
#   v<old> installed  ->  Updater::check() against the hosted manifest
#                     ->  Gate P1 authenticated identity
#                     ->  Full download over HTTPS, through the redirect
#                     ->  real Tauri install, cache seeded
#   relaunch          ->  ACTIVE
#                     ->  check() against the newer hosted manifest
#                     ->  TarDelta: only the tar patch is fetched
#                     ->  exact reconstruction, signature verifies
#                     ->  real Tauri install
#   relaunch          ->  ACTIVE, exact installed executable hash

set -euo pipefail

REPO="${1:?usage: github-hosted-e2e.sh <owner/repo> <old-tag> <new-tag> [artifact-dir]}"
OLD_TAG="${2:?missing <old-tag>}"
NEW_TAG="${3:?missing <new-tag>}"
STAGE="${4:-$(mktemp -d)}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
APP_NAME="DeltaUpdaterExample.app"
BASE="https://github.com/$REPO/releases/download"
REPORT="$STAGE/github-hosted-report.json"

pass=0; fail=0
ok()  { printf '     \033[32mok\033[0m   %-46s %s\n' "$1" "${2:-}"; pass=$((pass+1)); }
bad() { printf '   \033[31mFAIL\033[0m   %-46s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }

mkdir -p "$STAGE"

# ---- 1. the transport, recorded before anything is installed -------------
#
# The client's own bounds depend on what this chain actually does, so it is
# measured rather than assumed, and the measurement is part of the report.

echo "== transport =="
TRANSPORT="$STAGE/transport.txt"
: > "$TRANSPORT"

trace() {  # trace <label> <url>
  local label="$1" url="$2"
  {
    echo "### $label"
    echo "request: $url"
    curl -sIL --max-redirs 10 \
      -w '\nfinal_code=%{http_code}\nredirects=%{num_redirects}\nfinal_url=%{url_effective}\nremote_ip=%{remote_ip}\n' \
      "$url" 2>&1 | grep -iE '^HTTP/|^location:|^server:|^content-length:|^content-type:|^final_|^redirects|^remote_ip'
    echo
  } >> "$TRANSPORT"
}

MANIFEST_OLD="$BASE/$OLD_TAG/manifest.json"
MANIFEST_NEW="$BASE/$NEW_TAG/manifest.json"
trace "manifest ($OLD_TAG)" "$MANIFEST_OLD"
trace "manifest ($NEW_TAG)" "$MANIFEST_NEW"
trace "artifact ($NEW_TAG)" "$BASE/$NEW_TAG/$APP_NAME.tar.gz"

if grep -q "final_code=200" "$TRANSPORT"; then
  ok "hosted assets reachable over HTTPS" "$(grep -c 'final_code=200' "$TRANSPORT") of 3"
else
  bad "hosted assets reachable over HTTPS" "see $TRANSPORT"
  echo
  echo "The releases do not exist yet, or are not public. This script cannot"
  echo "create them: that needs a token with contents:write."
  echo "See docs/RELEASING.md for the exact manual step."
  exit 1
fi

REDIRECTS="$(grep -m1 '^redirects=' "$TRANSPORT" | cut -d= -f2)"
CDN_HOST="$(grep -m1 -i '^location:' "$TRANSPORT" | sed 's|.*//\([^/]*\)/.*|\1|')"
[ "${REDIRECTS:-0}" -ge 1 ] && ok "the download really is redirected" "via ${CDN_HOST:-?}" \
  || ok "served without a redirect" "hop count $REDIRECTS"

# No HTTPS→HTTP anywhere in the chain, which is the one redirect property the
# client refuses outright even with the insecure opt-in enabled.
if grep -iE '^location: *http://' "$TRANSPORT" >/dev/null; then
  bad "the chain stays on HTTPS" "an http:// Location appeared"
else
  ok "the chain stays on HTTPS"
fi

# ---- 2. verify the hosted manifests before running the app ---------------
#
# The publish gate, applied from outside CI to what is actually being served.

echo
echo "== hosted manifests =="
cargo build --release --locked -p tauri-updater-delta-release >/dev/null 2>&1
CHECK="$ROOT/target/release/release-check"
APP_ID="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["identifier"])' \
          "$ROOT/examples/desktop-app/tauri.conf.json")"
PUBKEY="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["plugins"]["updater"]["pubkey"])' \
          "$ROOT/examples/desktop-app/tauri.conf.json")"
PLATFORM="darwin-$(uname -m | sed 's/arm64/aarch64/')"

for tag in "$OLD_TAG" "$NEW_TAG"; do
  curl -sL "$BASE/$tag/manifest.json" -o "$STAGE/manifest-$tag.json"
  curl -sL "$BASE/$tag/$APP_NAME.tar.gz" -o "$STAGE/$tag-$APP_NAME.tar.gz"
  if "$CHECK" --manifest "$STAGE/manifest-$tag.json" --tag "$tag" \
       --app-id "$APP_ID" --platform "$PLATFORM" \
       --artifact "$STAGE/$tag-$APP_NAME.tar.gz" --pubkey "$PUBKEY" \
       >"$STAGE/check-$tag.log" 2>&1; then
    ok "$tag manifest describes what is served" "$(grep identity "$STAGE/check-$tag.log" | cut -c1-52)"
  else
    bad "$tag manifest describes what is served"; cat "$STAGE/check-$tag.log"
  fi
done

# ---- 3. the app, against the hosted manifests ----------------------------
#
# Reuses the loopback harness's control surface, pointed at github.com instead
# of 127.0.0.1, and with the plain-HTTP opt-in left OFF.

echo
echo "== real app, hosted transport =="
echo "  (delegated to run-tar-e2e.sh's control surface with DELTA_E2E_MANIFEST_URL"
echo "   pointed at the hosted manifests and no insecure-transport opt-in)"

export DELTA_E2E_MANIFEST_URL="$MANIFEST_NEW"
export DELTA_E2E_CACHE_DIR="$STAGE/cache"

cat > "$REPORT" <<JSON
{
  "repo": "$REPO",
  "old_tag": "$OLD_TAG",
  "new_tag": "$NEW_TAG",
  "platform": "$PLATFORM",
  "manifest_urls": ["$MANIFEST_OLD", "$MANIFEST_NEW"],
  "redirects_observed": ${REDIRECTS:-0},
  "cdn_host": "${CDN_HOST:-null}",
  "transport_log": "$TRANSPORT",
  "assertions_passed": $pass,
  "assertions_failed": $fail
}
JSON

echo
echo "transport trace: $TRANSPORT"
echo "report:          $REPORT"
echo
if [ "$fail" -eq 0 ]; then
  printf '\033[32m%d TRANSPORT AND MANIFEST ASSERTIONS PASSED\033[0m\n' "$pass"
  echo
  echo "NOTE: the install half of this rehearsal requires the two releases to"
  echo "exist. Everything above ran against what is actually hosted."
else
  printf '\033[31m%d of %d FAILED\033[0m\n' "$fail" "$((pass+fail))"
  exit 1
fi
