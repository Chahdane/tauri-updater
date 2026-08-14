#!/usr/bin/env bash
# Build three real versions of the example app and publish two releases.
#
# Everything the cache-backed end-to-end needs, produced the way a real release
# would be: `cargo tauri build` for the bundles, `delta-release` for the
# patches, digests, signature, tar layer and manifest. Nothing is hand-crafted,
# and in particular no manifest field is written by this script.
#
#   ./e2e/build-three-versions.sh [output-dir]
#
# Output (default /private/tmp/delta-tar-e2e):
#   v1.0.0/  v1.0.1/  v1.0.2/     bundle and .app.tar.gz for each
#   manifest-1.0.1.json           1.0.0 -> 1.0.1, direct patch and tar layer
#   manifest-1.0.2.json           1.0.1 -> 1.0.2, direct patch and tar layer
#   patch-*.zst, tar-*.zst        the patches those manifests name
#   key, key.pub                  generated per run, never committed
#   versions.json                 measurements, for the research record
#
# Three versions rather than two because the cache state machine cannot be
# exercised with fewer: one transition fills the cache, the second uses it. A
# two-version run can only ever demonstrate the first.

set -euo pipefail

_ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
_LOCK="$_ROOT_DIR/Cargo.lock"
_LOCK_BACKUP="$(mktemp)"
[ -f "$_LOCK" ] && cp "$_LOCK" "$_LOCK_BACKUP"
_restore_lock() {
  if [ -s "$_LOCK_BACKUP" ]; then
    cp "$_LOCK_BACKUP" "$_LOCK"
    rm -f "$_LOCK_BACKUP"
  fi
}

OUT="${1:-/private/tmp/delta-tar-e2e}"
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$APP_DIR/../.." && pwd)"
KEY_PASSWORD="e2e-test-password"
# Bound into the signature's authenticated release identity, and compared at
# runtime against the app's own tauri.conf.json identifier.
APP_ID="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["identifier"])' "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/tauri.conf.json")"
APP_NAME="DeltaUpdaterExample.app"
MAIN_BINARY="Contents/MacOS/delta-updater-example"

case "$OUT" in /tmp/*) echo "FATAL: use /private/tmp, not /tmp (symlink)" >&2; exit 1;; esac

ARCH="$(uname -m)"; [ "$ARCH" = "arm64" ] && ARCH="aarch64"
case "$(uname -s)" in
  Darwin) PLATFORM="darwin-$ARCH" ;;
  *)      echo "FATAL: the tar layer is macOS-only for now (got $(uname -s))" >&2; exit 1 ;;
esac

rm -rf "$OUT"; mkdir -p "$OUT"

echo "==> generating a signing key (this run only)"
cargo tauri signer generate --ci --password "$KEY_PASSWORD" \
  --write-keys "$OUT/key" --force >/dev/null 2>&1
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$OUT/key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD"

cleanup() {
  git -C "$ROOT" checkout -- "$APP_DIR/tauri.conf.json" "$APP_DIR/Cargo.toml" 2>/dev/null || true
  _restore_lock
}
trap cleanup EXIT INT TERM

build_version() {
  local version="$1"
  echo "==> building $version"
  mkdir -p "$OUT/v$version"
  python3 - "$APP_DIR" "$version" "$OUT/key.pub" <<'PY'
import json, sys, re, pathlib
app_dir, version, pub_path = sys.argv[1], sys.argv[2], sys.argv[3]
conf = pathlib.Path(app_dir, "tauri.conf.json")
cfg = json.loads(conf.read_text())
cfg["version"] = version
cfg["plugins"]["updater"]["pubkey"] = pathlib.Path(pub_path).read_text().strip()
# Tauri refuses non-HTTPS updater endpoints at config load. The harness serves
# over plain HTTP on loopback, so this flag is required -- and it is set HERE,
# never in the committed config, so the example app stays secure by default.
cfg["plugins"]["updater"]["dangerousInsecureTransportProtocol"] = True
conf.write_text(json.dumps(cfg, indent=2) + "\n")

cargo = pathlib.Path(app_dir, "Cargo.toml")
cargo.write_text(re.sub(r'^version = "[^"]+"$', f'version = "{version}"',
                        cargo.read_text(), count=1, flags=re.M))
PY
  ( cd "$APP_DIR" && cargo tauri build --features e2e-control >/dev/null 2>&1 )
  local bundle="$ROOT/target/release/bundle/macos"
  cp -R "$bundle/$APP_NAME" "$OUT/v$version/"
  cp "$bundle/$APP_NAME.tar.gz" "$OUT/v$version/"
}

build_version 1.0.0
build_version 1.0.1
build_version 1.0.2

# The fixture-difference discipline, at the layer that matters. Different
# metadata wrapped around an identical binary would make every downstream
# assertion vacuous, so this is checked on the MAIN BINARY -- and now across
# three versions, so a pair that happens to differ cannot hide a pair that does
# not.
declare -a HASHES
for v in 1.0.0 1.0.1 1.0.2; do
  HASHES+=("$(shasum -a 256 "$OUT/v$v/$APP_NAME/$MAIN_BINARY" | awk '{print $1}')")
done
for i in 0 1 2; do
  for j in 0 1 2; do
    if [ $i -lt $j ] && [ "${HASHES[$i]}" = "${HASHES[$j]}" ]; then
      echo "FATAL: two of the three versions have an identical main binary." >&2
      echo "       Every end-to-end assertion below would pass vacuously." >&2
      exit 1
    fi
  done
done
echo "==> three distinct main binaries:"
for i in 0 1 2; do echo "    ${HASHES[$i]}"; done

echo "==> building delta-release"
cargo build -q --release -p tauri-updater-delta-release --manifest-path "$ROOT/Cargo.toml"

# The URLs are placeholders; the runner rewrites them once it knows the port.
publish() {
  local from="$1" to="$2"
  echo "==> publishing $from -> $to"
  "$ROOT/target/release/delta-release" \
    --platform "$PLATFORM" \
    --app-id "$APP_ID" \
    --target-version "$to" --from-version "$from" \
    --previous-installer "$OUT/v$from/$APP_NAME.tar.gz" \
    --new-installer      "$OUT/v$to/$APP_NAME.tar.gz" \
    --installer-url "http://127.0.0.1:0/v$to/$APP_NAME.tar.gz" \
    --patch-url     "http://127.0.0.1:0/patch-$from-$to.zst" \
    --patch-out     "$OUT/patch-$from-$to.zst" \
    --tar-patch-url "http://127.0.0.1:0/tar-$from-$to.zst" \
    --tar-patch-out "$OUT/tar-$from-$to.zst" \
    --require-tar-layer \
    --manifest      "$OUT/manifest-$to.json"
}

# --require-tar-layer, deliberately. A missing tar layer is invisible in a
# manifest -- the release looks entirely successful and every client silently
# does the expensive thing -- so the run must fail loudly instead.
publish 1.0.0 1.0.1
publish 1.0.1 1.0.2

echo "==> recording measurements"
python3 - "$OUT" "$PLATFORM" "${HASHES[0]}" "${HASHES[1]}" "${HASHES[2]}" <<'PY'
import json, os, subprocess, sys, gzip, hashlib, pathlib
out, platform = sys.argv[1], sys.argv[2]
hashes = sys.argv[3:6]

def blake3(path):
    # No blake3 in the stdlib; use the digests delta-release already wrote.
    return None

def size(p):
    return os.path.getsize(p)

def tar_size(p):
    n = 0
    with gzip.open(p, "rb") as f:
        while True:
            chunk = f.read(1 << 20)
            if not chunk:
                break
            n += len(chunk)
    return n

record = {"platform": platform, "versions": {}, "releases": {}}
for i, v in enumerate(["1.0.0", "1.0.1", "1.0.2"]):
    artifact = f"{out}/v{v}/DeltaUpdaterExample.app.tar.gz"
    record["versions"][v] = {
        "main_binary_sha256": hashes[i],
        "artifact_size": size(artifact),
        "artifact_sha256": hashlib.sha256(open(artifact, "rb").read()).hexdigest(),
        "tar_size": tar_size(artifact),
    }

for frm, to in [("1.0.0", "1.0.1"), ("1.0.1", "1.0.2")]:
    manifest = json.load(open(f"{out}/manifest-{to}.json"))
    entry = manifest["delta"]["platforms"][platform]
    tar_layer = entry["tar_layer"]
    direct = entry["patches"][frm]
    tar_patch = tar_layer["patches"][frm]
    installer = entry["target_installer_size"]
    record["releases"][f"{frm}->{to}"] = {
        "installer_size": installer,
        "direct_patch_size": direct["patch_size"],
        "direct_patch_percent": round(direct["patch_size"] / installer * 100, 4),
        "tar_patch_size": tar_patch["patch_size"],
        "tar_patch_percent": round(tar_patch["patch_size"] / installer * 100, 4),
        "reduction_percent": round(
            (1 - tar_patch["patch_size"] / direct["patch_size"]) * 100, 4
        ),
        "target_tar_size": tar_layer["target_tar_size"],
        "target_tar_blake3": tar_layer["target_tar_blake3"],
        "target_installer_blake3": entry["target_installer_blake3"],
        "base_tar_blake3": tar_patch["base_tar_blake3"],
        "representation": tar_layer["representation"],
        "recompression": tar_layer["recompression"],
    }

json.dump(record, open(f"{out}/versions.json", "w"), indent=2)
for name, r in record["releases"].items():
    print(f"    {name}: direct {r['direct_patch_size']} ({r['direct_patch_percent']}%), "
          f"tar {r['tar_patch_size']} ({r['tar_patch_percent']}%), "
          f"{r['reduction_percent']}% fewer patch bytes")
PY

echo
echo "==> ready in $OUT (platform $PLATFORM)"
