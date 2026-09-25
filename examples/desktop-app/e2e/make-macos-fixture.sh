#!/usr/bin/env bash
# Regenerate crates/delta-core/tests/fixtures/macos-app-tar-gz from a real build.
#
#   ./e2e/make-macos-fixture.sh <output-dir>
#
# Builds examples/desktop-app 1.0.1 with the tauri-cli on PATH (the workflows
# install tauri-cli 2.10.1 with --locked) and writes the fixture files: the
# official .app.tar.gz, and official-1.0.1.json with its BLAKE3, size, the
# signature tauri-bundler issued, and the public key. The private key is
# generated for this run only and never written to the output.
#
# The recompression recipe must rebuild this file byte-for-byte, so it pins the
# compressor tauri-bundler used (DECISIONS #42). toolchain.txt records it.

set -euo pipefail

OUT="$1"
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$APP_DIR/../.." && pwd)"
KEYS="$(mktemp -d)"
trap 'rm -rf "$KEYS"; git -C "$ROOT" checkout -- "$APP_DIR/tauri.conf.json" "$APP_DIR/Cargo.toml" 2>/dev/null || true' EXIT

[ "$(uname -s)" = Darwin ] || { echo "FATAL: builds a macOS .app.tar.gz" >&2; exit 1; }
mkdir -p "$OUT"

cargo tauri signer generate --ci --password fixture-password \
  --write-keys "$KEYS/key" --force >/dev/null 2>&1
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEYS/key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=fixture-password

python3 - "$APP_DIR" "$KEYS/key.pub" <<'PY'
import json, pathlib, re, sys
app, pub = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
conf = app / "tauri.conf.json"
cfg = json.loads(conf.read_text())
cfg["version"] = "1.0.1"
cfg["plugins"]["updater"]["pubkey"] = pub.read_text().strip()
conf.write_text(json.dumps(cfg, indent=2) + "\n")
cargo = app / "Cargo.toml"
cargo.write_text(re.sub(r'^version = "[^"]+"$', 'version = "1.0.1"',
                        cargo.read_text(), count=1, flags=re.M))
PY

rm -rf "$ROOT/target/release/bundle/macos"
( cd "$APP_DIR" && cargo tauri build )
bundle="$ROOT/target/release/bundle/macos/DeltaUpdaterExample.app.tar.gz"
cp "$bundle" "$OUT/official-1.0.1.app.tar.gz"

python3 -m pip install --quiet blake3
python3 - "$OUT" "$bundle.sig" "$KEYS/key.pub" <<'PY'
import json, os, sys, blake3
out, sig, pub = sys.argv[1:4]
art = f"{out}/official-1.0.1.app.tar.gz"
data = open(art, "rb").read()
json.dump({
    "artifact": "official-1.0.1.app.tar.gz",
    "version": "1.0.1",
    "platform_key": "darwin-aarch64",
    "installer_blake3": blake3.blake3(data).hexdigest(),
    "installer_size": len(data),
    "signature": open(sig).read().strip(),
    "pubkey": open(pub).read().strip(),
}, open(f"{out}/official-1.0.1.json", "w"), indent=2)
print(json.load(open(f"{out}/official-1.0.1.json"))["installer_blake3"], len(data))
PY

{
  echo "rustc: $(rustc -V)"
  echo "tauri-cli: $(cargo tauri --version)"
  lock="$(ls -d "$HOME"/.cargo/registry/src/*/tauri-cli-2.10.1/Cargo.lock 2>/dev/null | head -1)"
  for crate in flate2 zlib-rs libz-rs-sys tar; do
    echo "$crate: $(grep -A1 "^name = \"$crate\"$" "$lock" | sed -n 2p | cut -d'"' -f2)"
  done
} | tee "$OUT/toolchain.txt"
