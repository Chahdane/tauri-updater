#!/usr/bin/env bash
# Run the release workflow's command sequence locally, against real bundles.
#
# The workflow is the one part of this system that runs once per release, on a
# machine nobody is watching, whose failures are discovered by users. Blocker B5
# is what that looks like: a branch nothing had ever executed published an
# artifact with no updater document and no signature.
#
# So the sequence is rehearsed here, with the same binaries and the same
# arguments the workflow passes, over `cargo tauri build` output. What is *not*
# rehearsed is `gh` -- no network, no token, no upload. Everything up to the
# upload is real.
#
#   ./rehearse-release.sh <artifact-dir>
#
# where <artifact-dir> holds v<version>/DeltaUpdaterExample.app.tar.gz for at
# least two versions, as examples/desktop-app/e2e/build-three-versions.sh
# produces.
#
# Covers:
#   State A  no predecessor                 -> signed Full-only release
#   State B  predecessor with no artifact   -> signed Full-only release
#   State C  usable predecessor             -> Full + direct + tar-layer patches
#
# Each state ends at `release-check`, which is the same gate the workflow runs
# immediately before uploading.

set -euo pipefail

ARTIFACTS="${1:?usage: rehearse-release.sh <artifact-dir>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
APP_DIR="$ROOT/examples/desktop-app"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

APP_ID="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["identifier"])' "$APP_DIR/tauri.conf.json")"
PLATFORM="darwin-$(uname -m | sed 's/arm64/aarch64/')"
BASE="https://github.com/example/example/releases/download"

pass=0
fail=0
ok()   { printf '     \033[32mok\033[0m   %-52s %s\n' "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf '   \033[31mFAIL\033[0m   %-52s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
note() { printf '     --   %s\n' "$1"; }

echo "== building the release tooling =="
cargo build --release --locked -p tauri-updater-delta-release >/dev/null 2>&1
RELEASE="$ROOT/target/release/delta-release"
CHECK="$ROOT/target/release/release-check"

# A throwaway key, as the workflow gets from its repository secret.
#
# A real password, deliberately. `tauri signer generate --password ""` produces
# a key this tooling cannot read at all -- Tauri uses rsign2, which encrypts even
# with an empty password, while this uses the minisign crate, which does not.
# See docs/DECISIONS.md #16. The same trap the release workflow documents.
echo "== generating a rehearsal signing key =="
KEY_PASSWORD="rehearsal-password"
KEYDIR="$WORK/key"
mkdir -p "$KEYDIR"
cargo tauri signer generate --ci --password "$KEY_PASSWORD" \
  --write-keys "$KEYDIR/key" --force >/dev/null 2>&1 || {
    echo "could not generate a signing key with 'cargo tauri signer generate'." >&2
    echo "Install the Tauri CLI: cargo install tauri-cli --version '=2.10.1' --locked" >&2
    exit 1
  }
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEYDIR/key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD"
# `tauri signer generate` writes the public key already in the form
# tauri.conf.json carries, so it is used verbatim rather than re-encoded.
PUBKEY_B64="$(tr -d '\n' < "$KEYDIR/key.pub")"

versions=()
for d in "$ARTIFACTS"/v*/; do
  v="$(basename "$d")"; versions+=("${v#v}")
done
IFS=$'\n' versions=($(sort -V <<<"${versions[*]}")); unset IFS
if [ "${#versions[@]}" -lt 2 ]; then
  echo "need at least two built versions in $ARTIFACTS" >&2
  exit 1
fi
OLD="${versions[0]}"
NEW="${versions[1]}"
OLD_ART="$ARTIFACTS/v$OLD/DeltaUpdaterExample.app.tar.gz"
NEW_ART="$ARTIFACTS/v$NEW/DeltaUpdaterExample.app.tar.gz"
note "predecessor $OLD, release $NEW, platform $PLATFORM"

run_check() {  # run_check <dist> <tag> <artifact>
  "$CHECK" \
    --manifest "$1/manifest.json" \
    --tag "$2" \
    --app-id "$APP_ID" \
    --platform "$PLATFORM" \
    --artifact "$3" \
    --pubkey "$PUBKEY_B64"
}

# ---- State A: no predecessor --------------------------------------------
echo
echo "== state A: no previous release =="
DIST_A="$WORK/dist-a"; mkdir -p "$DIST_A"
cp "$OLD_ART" "$DIST_A/"
ART_A="$DIST_A/$(basename "$OLD_ART")"

"$RELEASE" \
  --platform "$PLATFORM" \
  --app-id "$APP_ID" \
  --target-version "$OLD" \
  --new-installer "$ART_A" \
  --installer-url "$BASE/v$OLD/$(basename "$ART_A")" \
  --manifest "$DIST_A/manifest.json" \
  --signature-out "$ART_A.sig" \
  --pub-date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$WORK/a.log" 2>&1 \
  && ok "a first release generates" \
  || { bad "a first release generates"; cat "$WORK/a.log"; }

[ -f "$DIST_A/manifest.json" ] && ok "manifest.json exists" \
  || bad "manifest.json exists" "B5: this is what used to be missing"
[ -f "$ART_A.sig" ] && ok "signature file exists" || bad "signature file exists"

if run_check "$DIST_A" "v$OLD" "$ART_A" >"$WORK/a-check.log" 2>&1; then
  ok "publish gate accepts it" "$(grep identity "$WORK/a-check.log" | head -1 | cut -c1-60)"
else
  bad "publish gate accepts it"; cat "$WORK/a-check.log"
fi

python3 - "$DIST_A/manifest.json" "$PLATFORM" <<'PY' && ok "no fabricated delta entries" || bad "no fabricated delta entries"
import json,sys
m=json.load(open(sys.argv[1])); plat=sys.argv[2]
d=m.get("delta",{}).get("platforms",{}).get(plat,{})
sys.exit(0 if not d.get("patches") and not d.get("tar_layer") else 1)
PY

# ---- State B: predecessor exists, artifact unusable ----------------------
echo
echo "== state B: previous release has no usable artifact =="
DIST_B="$WORK/dist-b"; mkdir -p "$DIST_B"
cp "$NEW_ART" "$DIST_B/"
ART_B="$DIST_B/$(basename "$NEW_ART")"

# The workflow's answer to a failed download is to pass no predecessor.
"$RELEASE" \
  --platform "$PLATFORM" \
  --app-id "$APP_ID" \
  --target-version "$NEW" \
  --new-installer "$ART_B" \
  --installer-url "$BASE/v$NEW/$(basename "$ART_B")" \
  --manifest "$DIST_B/manifest.json" \
  --signature-out "$ART_B.sig" >"$WORK/b.log" 2>&1 \
  && ok "an unusable predecessor still releases" \
  || { bad "an unusable predecessor still releases"; cat "$WORK/b.log"; }

run_check "$DIST_B" "v$NEW" "$ART_B" >/dev/null 2>&1 \
  && ok "publish gate accepts it" || bad "publish gate accepts it"

# ---- State C: a usable predecessor ---------------------------------------
echo
echo "== state C: a usable previous release =="
DIST_C="$WORK/dist-c"; mkdir -p "$DIST_C"
cp "$NEW_ART" "$DIST_C/"
ART_C="$DIST_C/$(basename "$NEW_ART")"
PATCH="$DIST_C/$OLD-to-$NEW.zst"
TARPATCH="$DIST_C/$OLD-to-$NEW.tar.zst"

"$RELEASE" \
  --platform "$PLATFORM" \
  --app-id "$APP_ID" \
  --target-version "$NEW" \
  --from-version "$OLD" \
  --previous-installer "$OLD_ART" \
  --new-installer "$ART_C" \
  --installer-url "$BASE/v$NEW/$(basename "$ART_C")" \
  --patch-url "$BASE/v$NEW/$(basename "$PATCH")" \
  --patch-out "$PATCH" \
  --tar-patch-url "$BASE/v$NEW/$(basename "$TARPATCH")" \
  --tar-patch-out "$TARPATCH" \
  --require-tar-layer \
  --manifest "$DIST_C/manifest.json" \
  --signature-out "$ART_C.sig" >"$WORK/c.log" 2>&1 \
  && ok "a delta release generates" \
  || { bad "a delta release generates"; cat "$WORK/c.log"; }

[ -f "$PATCH" ]    && ok "direct patch written"    "$(wc -c <"$PATCH" | tr -d ' ') bytes"
[ -f "$TARPATCH" ] && ok "tar-layer patch written" "$(wc -c <"$TARPATCH" | tr -d ' ') bytes"
grep -q "round-tripped to the exact published artifact" "$WORK/c.log" \
  && ok "tar layer round-tripped at release time" \
  || bad "tar layer round-tripped at release time"

if run_check "$DIST_C" "v$NEW" "$ART_C" >"$WORK/c-check.log" 2>&1; then
  ok "publish gate accepts it" "$(grep 'tar layer' "$WORK/c-check.log" | head -1)"
else
  bad "publish gate accepts it"; cat "$WORK/c-check.log"
fi

# ---- the gate actually refuses -------------------------------------------
echo
echo "== the publish gate refuses what it should =="
run_check "$DIST_C" "v9.9.9" "$ART_C" >/dev/null 2>&1 \
  && bad "a wrong tag is refused" || ok "a wrong tag is refused"

"$CHECK" --manifest "$DIST_C/manifest.json" --tag "v$NEW" --app-id "com.example.wrong" \
  --platform "$PLATFORM" --artifact "$ART_C" --pubkey "$PUBKEY_B64" >/dev/null 2>&1 \
  && bad "a wrong app id is refused" || ok "a wrong app id is refused"

run_check "$DIST_C" "v$NEW" "$OLD_ART" >/dev/null 2>&1 \
  && bad "the wrong artifact is refused" || ok "the wrong artifact is refused"

# ---- the published asset set ---------------------------------------------
echo
echo "== published asset set (state C) =="
( cd "$DIST_C" && ls -1 ) | sed 's/^/     /'

echo
if [ "$fail" -eq 0 ]; then
  printf '\033[32mALL %d REHEARSAL ASSERTIONS PASSED\033[0m\n' "$pass"
else
  printf '\033[31m%d of %d FAILED\033[0m\n' "$fail" "$((pass+fail))"
  exit 1
fi
