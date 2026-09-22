#!/usr/bin/env bash
# Build three real Windows versions of the example app and publish two releases.
#
# The Windows counterpart of build-three-versions.sh, and deliberately the same
# shape: `cargo tauri build` for the bundles, `delta-release` for the patches,
# digests, signature and manifest. Nothing is hand-crafted and no manifest field
# is written by this script.
#
#   ./e2e/build-three-versions-windows.sh [output-dir]
#
# Output (default /c/delta-nsis-e2e):
#   v1.0.0/  v1.0.1/  v1.0.2/     the NSIS -setup.exe for each
#   manifest-1.0.1.json           1.0.0 -> 1.0.1, direct patch
#   manifest-1.0.2.json           1.0.1 -> 1.0.2, direct patch
#   patch-*.zst                   the patches those manifests name
#   key, key.pub                  generated per run, never committed
#   versions.json                 measurements, for the research record
#
# ## No tar layer, and that is the point
#
# An NSIS installer is not a gzipped tarball, so there is no inner tar to patch
# and nothing that could rebuild the published bytes from one. The release
# carries an `opaque-v1` direct patch, and the client's job is to hold the exact
# official installer so that patch has the base it was generated against.
#
# The ratio that produces is a **measurement**, not a target. NSIS compresses
# solid with LZMA by default, so a small source change can move most of the
# archive. versions.json records what it actually was; nothing here assumes it
# is good.

set -euo pipefail

OUT="${1:-/c/delta-nsis-e2e}"
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$APP_DIR/../.." && pwd)"
KEY_PASSWORD="e2e-test-password"
PRODUCT="DeltaUpdaterExample"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) ;;
  *) echo "FATAL: this harness builds NSIS installers and needs Windows (got $(uname -s))" >&2; exit 1 ;;
esac

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) PLATFORM="windows-x86_64" ;;
  aarch64|arm64) PLATFORM="windows-aarch64" ;;
  *) echo "FATAL: unsupported architecture $ARCH" >&2; exit 1 ;;
esac

# Bound into the signature's authenticated release identity, and compared at
# runtime against the app's own tauri.conf.json identifier.
# The application identifier and the version both come from
# tauri.conf.json via --app-config, which also refuses to build a release
# whose target version is not the one compiled into the app. Reading the
# identifier here and never reading the version is how a tag could
# publish a differently versioned application -- audit finding A-2.

_LOCK="$ROOT/Cargo.lock"
_LOCK_BACKUP="$(mktemp)"
cp "$_LOCK" "$_LOCK_BACKUP"
cleanup() {
  git -C "$ROOT" checkout -- "$APP_DIR/tauri.conf.json" "$APP_DIR/Cargo.toml" 2>/dev/null || true
  [ -s "$_LOCK_BACKUP" ] && cp "$_LOCK_BACKUP" "$_LOCK"
  rm -f "$_LOCK_BACKUP"
}
trap cleanup EXIT INT TERM

rm -rf "$OUT"; mkdir -p "$OUT"

echo "==> generating a signing key (this run only)"
cargo tauri signer generate --ci --password "$KEY_PASSWORD" \
  --write-keys "$OUT/key" --force >/dev/null 2>&1
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$OUT/key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD"

build_version() {
  local version="$1"
  echo "==> building $version"
  mkdir -p "$OUT/v$version"
  python - "$APP_DIR" "$version" "$OUT/key.pub" <<'PY'
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
  ( cd "$APP_DIR" && cargo tauri build --features e2e-control )
  local bundle="$ROOT/target/release/bundle/nsis"
  shopt -s nullglob
  local setups=("$bundle"/*-setup.exe)
  if [ "${#setups[@]}" -ne 1 ]; then
    echo "FATAL: expected exactly one -setup.exe, found ${#setups[@]} in $bundle" >&2
    exit 1
  fi
  cp "${setups[0]}" "$OUT/v$version/$PRODUCT-setup.exe"
  # The executable the installer will place, kept so the harness can assert on
  # the exact bytes that end up installed rather than on a version string the
  # app reports about itself.
  cp "$ROOT/target/release/$PRODUCT.exe" "$OUT/v$version/$PRODUCT.exe" 2>/dev/null \
    || cp "$ROOT/target/release/delta-updater-example.exe" "$OUT/v$version/$PRODUCT.exe"
}

build_version 1.0.0
build_version 1.0.1
build_version 1.0.2

# The fixture-difference discipline. Different metadata wrapped around an
# identical binary would make every downstream assertion vacuous, so this is
# checked on the MAIN BINARY -- across all three, so a pair that happens to
# differ cannot hide a pair that does not.
declare -a HASHES
for v in 1.0.0 1.0.1 1.0.2; do
  HASHES+=("$(sha256sum "$OUT/v$v/$PRODUCT.exe" | awk '{print $1}')")
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
  "$ROOT/target/release/delta-release.exe" \
    --platform "$PLATFORM" \
    --app-config "$APP_DIR/tauri.conf.json" \
    --target-version "$to" --from-version "$from" \
    --previous-installer "$OUT/v$from/$PRODUCT-setup.exe" \
    --new-installer      "$OUT/v$to/$PRODUCT-setup.exe" \
    --installer-url "http://127.0.0.1:0/v$to/$PRODUCT-setup.exe" \
    --patch-url     "http://127.0.0.1:0/patch-$from-$to.zst" \
    --patch-out     "$OUT/patch-$from-$to.zst" \
    --dangerously-allow-loopback-http-urls \
    --manifest      "$OUT/manifest-$to.json"
}

# No --require-tar-layer, and no --tar-patch-*: an NSIS installer has no inner
# tar this build can rebuild byte-for-byte, so asking for one would fail the
# release for doing exactly what it should.
publish 1.0.0 1.0.1
publish 1.0.1 1.0.2

echo "==> recording measurements"
python - "$OUT" "$PLATFORM" "$PRODUCT" "${HASHES[0]}" "${HASHES[1]}" "${HASHES[2]}" <<'PY'
import hashlib, json, os, sys

out, platform, product = sys.argv[1], sys.argv[2], sys.argv[3]
hashes = sys.argv[4:7]

def size(p):
    return os.path.getsize(p)

record = {"platform": platform, "installer": "nsis", "versions": {}, "releases": {}}
for i, v in enumerate(["1.0.0", "1.0.1", "1.0.2"]):
    setup = f"{out}/v{v}/{product}-setup.exe"
    record["versions"][v] = {
        "main_binary_sha256": hashes[i],
        "installer_size": size(setup),
        "installer_sha256": hashlib.sha256(open(setup, "rb").read()).hexdigest(),
    }

for frm, to in [("1.0.0", "1.0.1"), ("1.0.1", "1.0.2")]:
    manifest = json.load(open(f"{out}/manifest-{to}.json"))
    entry = manifest["delta"]["platforms"][platform]
    direct = entry["patches"][frm]
    installer = entry["target_installer_size"]
    record["releases"][f"{frm}->{to}"] = {
        "installer_size": installer,
        "direct_patch_size": direct["patch_size"],
        "direct_patch_percent": round(direct["patch_size"] / installer * 100, 4),
        "target_installer_blake3": entry["target_installer_blake3"],
        "base_installer_blake3": direct["base_installer_blake3"],
        "base_installer_size": direct["base_installer_size"],
        "representation": "opaque-v1",
    }

json.dump(record, open(f"{out}/versions.json", "w"), indent=2)
for name, r in record["releases"].items():
    print(f"    {name}: installer {r['installer_size']} bytes, "
          f"direct patch {r['direct_patch_size']} "
          f"({r['direct_patch_percent']}% of a full download)")
print()
print("    That percentage is a measurement of these two builds, not a claim")
print("    about any application. NSIS compresses solid with LZMA, so a small")
print("    source change can move most of the archive.")
PY

echo
echo "==> ready in $OUT (platform $PLATFORM)"
