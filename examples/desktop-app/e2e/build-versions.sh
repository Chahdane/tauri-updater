#!/usr/bin/env bash
# Build two real versions of the example app and publish them with delta-release.
#
# Everything the end-to-end needs, produced the way a real release would be:
# `cargo tauri build` for the bundles, `delta-release` for the patch, the
# digests, the signature and the manifest. Nothing here is hand-crafted.
#
#   ./e2e/build-versions.sh [output-dir]
#
# Output (default /tmp/delta-e2e-build):
#   v1.0.0/DeltaUpdaterExample.app          the installed app under test
#   v1.0.0/DeltaUpdaterExample.app.tar.gz   the base artifact for the patch
#   v1.0.1/…                                the release
#   patch.zst, manifest.json                published by delta-release
#   key, key.pub                            generated per run, never committed
#
# The signing key is generated fresh each run into the output directory. A
# test-only private key living in the repository is the same hazard class as a
# test-only HTTP surface that ships: safe by intent, dangerous by accident.
#
# Note the password. `tauri signer generate --password ""` produces a key this
# tooling cannot read — Tauri's CLI uses rsign2, which runs its KDF even over an
# empty password, while delta-release uses the minisign crate, which skips
# encryption when the password is empty. See docs/DECISIONS.md.

set -euo pipefail

OUT="${1:-/tmp/delta-e2e-build}"
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$APP_DIR/../.." && pwd)"
KEY_PASSWORD="e2e-test-password"
MAIN_BINARY="Contents/MacOS/delta-updater-example"

ARCH="$(uname -m)"; [ "$ARCH" = "arm64" ] && ARCH="aarch64"
case "$(uname -s)" in
  Darwin) PLATFORM="darwin-$ARCH" ;;
  Linux)  PLATFORM="linux-$ARCH" ;;
  *)      echo "unsupported platform: $(uname -s)" >&2; exit 1 ;;
esac

rm -rf "$OUT"; mkdir -p "$OUT/v1.0.0" "$OUT/v1.0.1"

echo "==> generating a signing key (this run only)"
cargo tauri signer generate --ci --password "$KEY_PASSWORD" \
  --write-keys "$OUT/key" --force >/dev/null 2>&1
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$OUT/key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD"

# Restore the tracked config whatever happens, so a failed run cannot leave the
# repository holding a generated public key.
cleanup() { git -C "$ROOT" checkout -- "$APP_DIR/tauri.conf.json" "$APP_DIR/Cargo.toml" 2>/dev/null || true; }
trap cleanup EXIT

build_version() {
  local version="$1"
  echo "==> building $version"
  python3 - "$APP_DIR" "$version" "$OUT/key.pub" <<'PY'
import json, sys, re, pathlib
app_dir, version, pub_path = sys.argv[1], sys.argv[2], sys.argv[3]
conf = pathlib.Path(app_dir, "tauri.conf.json")
cfg = json.loads(conf.read_text())
cfg["version"] = version
cfg["plugins"]["updater"]["pubkey"] = pathlib.Path(pub_path).read_text().strip()
# Tauri refuses non-HTTPS updater endpoints at config load. The harness serves
# over plain HTTP on loopback, so this flag is required — and it is set HERE,
# never in the committed config, so the example app stays secure by default.
# Same principle as the control surface and the signing key: a test-only
# relaxation must not be reachable in a normal build.
cfg["plugins"]["updater"]["dangerousInsecureTransportProtocol"] = True
conf.write_text(json.dumps(cfg, indent=2) + "\n")

cargo = pathlib.Path(app_dir, "Cargo.toml")
cargo.write_text(re.sub(r'^version = "[^"]+"$', f'version = "{version}"',
                        cargo.read_text(), count=1, flags=re.M))
PY
  ( cd "$APP_DIR" && cargo tauri build --features e2e-control >/dev/null 2>&1 )
  local bundle="$ROOT/target/release/bundle/macos"
  cp -R "$bundle/DeltaUpdaterExample.app" "$OUT/v$version/"
  cp "$bundle/DeltaUpdaterExample.app.tar.gz" "$OUT/v$version/"
}

build_version 1.0.0
build_version 1.0.1

# The fixture-difference discipline, at the layer that matters. Different
# metadata wrapped around an identical binary would make every downstream
# assertion vacuous, so this is checked on the MAIN BINARY, not the tarball.
old_hash="$(shasum -a 256 "$OUT/v1.0.0/DeltaUpdaterExample.app/$MAIN_BINARY" | awk '{print $1}')"
new_hash="$(shasum -a 256 "$OUT/v1.0.1/DeltaUpdaterExample.app/$MAIN_BINARY" | awk '{print $1}')"
if [ "$old_hash" = "$new_hash" ]; then
  echo "FATAL: the two versions have an identical main binary." >&2
  echo "       Every end-to-end assertion below would pass vacuously." >&2
  exit 1
fi
echo "==> main binaries differ: ${old_hash:0:16}… vs ${new_hash:0:16}…"

echo "==> publishing with delta-release"
cargo build -q --release -p tauri-updater-delta-release --manifest-path "$ROOT/Cargo.toml"
"$ROOT/target/release/delta-release" \
  --platform "$PLATFORM" \
  --version 1.0.1 --from-version 1.0.0 \
  --previous-installer "$OUT/v1.0.0/DeltaUpdaterExample.app.tar.gz" \
  --new-installer      "$OUT/v1.0.1/DeltaUpdaterExample.app.tar.gz" \
  --installer-url "http://127.0.0.1:0/DeltaUpdaterExample.app.tar.gz" \
  --patch-url     "http://127.0.0.1:0/patch.zst" \
  --patch-out     "$OUT/patch.zst" \
  --manifest      "$OUT/manifest.json"

echo
echo "==> ready in $OUT"
echo "    platform: $PLATFORM"
echo "    main binary v1.0.0: $old_hash"
echo "    main binary v1.0.1: $new_hash"
