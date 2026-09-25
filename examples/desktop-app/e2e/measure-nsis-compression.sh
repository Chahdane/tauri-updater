#!/usr/bin/env bash
# Measure how NSIS compression settings trade Full size against patch size.
#
#   ./e2e/measure-nsis-compression.sh [output-dir]
#
# For every variant below, builds the example app twice -- 1.0.0, then 1.0.1
# with a small feature-sized change -- and publishes the pair with the real
# `delta-release`. Records, per variant: both installer sizes, the direct patch
# size and ratio, and (for the uncompressed variant) what a compressed transport
# of the Full installer would cost. Output: <output-dir>/nsis-compression.json.
#
# Variants:
#   lzma-solid   Tauri's default (`SetCompressor /SOLID lzma`)
#   zlib-solid   `"compression": "zlib"` (Tauri also forces /SOLID)
#   none         `"compression": "none"`, what v0.1 ships
#   lzma-file    per-file LZMA: Tauri's own template with /SOLID removed.
#                Tauri's config cannot express this, so the template is taken
#                from the tauri-bundler source the pinned CLI was built from.
#
# The change between versions is a version bump plus a rewritten frontend page
# and a new ~64 KB script. Tauri embeds frontend assets in the executable, so
# this is closer to a real release than a version string alone -- and still not
# a real application's release. research/FINDINGS.md says which is which.
#
# A measurement, not a release: no --max-direct-patch-percent limit applies
# (it is set to 100) so every patch is kept and measured.

set -euo pipefail

OUT="${1:-/c/nsis-compression-study}"
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$APP_DIR/../.." && pwd)"
KEY_PASSWORD="study-password"
PRODUCT="DeltaUpdaterExample"
PLATFORM="windows-x86_64"
VARIANTS=(lzma-solid zlib-solid none lzma-file)

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) ;;
  *) echo "FATAL: NSIS builds need Windows (got $(uname -s))" >&2; exit 1 ;;
esac

# Everything this script edits is restored, whatever happens.
EDITED=("$APP_DIR/tauri.conf.json" "$APP_DIR/tauri.windows.conf.json"
        "$APP_DIR/Cargo.toml" "$APP_DIR/dist/index.html")
_LOCK_BACKUP="$(mktemp)"
cp "$ROOT/Cargo.lock" "$_LOCK_BACKUP"
cleanup() {
  git -C "$ROOT" checkout -- "${EDITED[@]}" 2>/dev/null || true
  rm -f "$APP_DIR/dist/changelog.js"
  [ -s "$_LOCK_BACKUP" ] && cp "$_LOCK_BACKUP" "$ROOT/Cargo.lock"
  rm -f "$_LOCK_BACKUP"
}
trap cleanup EXIT INT TERM

rm -rf "$OUT"; mkdir -p "$OUT"

# Per-file LZMA needs Tauri's template without /SOLID. Take it from the exact
# bundler source the pinned CLI compiled, never from a copy that could drift.
TEMPLATE_SRC="$(ls -d "$HOME"/.cargo/registry/src/*/tauri-bundler-2.8.1/src/bundle/windows/nsis/installer.nsi 2>/dev/null | head -1 || true)"
if [ -z "$TEMPLATE_SRC" ]; then
  echo "FATAL: tauri-bundler 2.8.1's installer.nsi is not in the cargo registry;" >&2
  echo "       install the pinned tauri-cli from source first." >&2
  exit 1
fi
grep -q 'SetCompressor /SOLID' "$TEMPLATE_SRC" \
  || { echo "FATAL: the template no longer says 'SetCompressor /SOLID'" >&2; exit 1; }
sed 's|SetCompressor /SOLID|SetCompressor|' "$TEMPLATE_SRC" > "$OUT/installer-per-file.nsi"

cargo tauri signer generate --ci --password "$KEY_PASSWORD" \
  --write-keys "$OUT/key" --force >/dev/null 2>&1
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$OUT/key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD"

cargo build -q --release -p tauri-updater-delta-release --manifest-path "$ROOT/Cargo.toml"

configure() {
  local variant="$1" version="$2"
  git -C "$ROOT" checkout -- "${EDITED[@]}"
  rm -f "$APP_DIR/dist/changelog.js"
  python - "$APP_DIR" "$variant" "$version" "$OUT" <<'PY'
import json, pathlib, re, sys
app, variant, version, out = sys.argv[1:5]
app = pathlib.Path(app)

conf = app / "tauri.conf.json"
cfg = json.loads(conf.read_text())
cfg["version"] = version
cfg["plugins"]["updater"]["pubkey"] = pathlib.Path(out, "key.pub").read_text().strip()
conf.write_text(json.dumps(cfg, indent=2) + "\n")

cargo = app / "Cargo.toml"
cargo.write_text(re.sub(r'^version = "[^"]+"$', f'version = "{version}"',
                        cargo.read_text(), count=1, flags=re.M))

win = app / "tauri.windows.conf.json"
wcfg = json.loads(win.read_text())
nsis = wcfg["bundle"]["windows"]["nsis"]
nsis["compression"] = {"lzma-solid": "lzma", "zlib-solid": "zlib",
                       "none": "none", "lzma-file": "lzma"}[variant]
if variant == "lzma-file":
    nsis["template"] = str(pathlib.Path(out, "installer-per-file.nsi"))
win.write_text(json.dumps(wcfg, indent=2) + "\n")

if version != "1.0.0":
    # The "feature" in this release: a changed page and a new script.
    index = app / "dist" / "index.html"
    before = index.read_text()
    index.write_text(before + '<p>What is new in this release.</p>\n'
                              '<script src="changelog.js"></script>\n')
    assert index.read_text() != before, "the release change did not apply"
    lines = [f"// entry {i}: fixed a thing in module {i % 97}" for i in range(1600)]
    (app / "dist" / "changelog.js").write_text("\n".join(lines) + "\n")
PY
}

build() {
  local variant="$1" version="$2" dest="$OUT/$variant/v$version"
  echo "==> $variant $version"
  configure "$variant" "$version"
  mkdir -p "$dest"
  cp "$APP_DIR/tauri.conf.json" "$APP_DIR/Cargo.toml" "$dest/"
  local bundle="$ROOT/target/release/bundle/nsis"
  rm -rf "$bundle"
  ( cd "$APP_DIR" && cargo tauri build )
  shopt -s nullglob
  local setups=("$bundle"/*-setup.exe)
  [ "${#setups[@]}" -eq 1 ] || { echo "FATAL: expected one -setup.exe in $bundle" >&2; exit 1; }
  cp "${setups[0]}" "$dest/$PRODUCT-setup.exe"
}

for variant in "${VARIANTS[@]}"; do
  build "$variant" 1.0.0
  build "$variant" 1.0.1
  "$ROOT/target/release/delta-release.exe" \
    --platform "$PLATFORM" \
    --app-config "$OUT/$variant/v1.0.1/tauri.conf.json" \
    --target-version 1.0.1 --from-version 1.0.0 \
    --previous-installer "$OUT/$variant/v1.0.0/$PRODUCT-setup.exe" \
    --new-installer "$OUT/$variant/v1.0.1/$PRODUCT-setup.exe" \
    --installer-url "https://example.invalid/$variant/setup.exe" \
    --patch-url "https://example.invalid/$variant/patch.zst" \
    --patch-out "$OUT/$variant/patch.zst" \
    --max-direct-patch-percent 100 \
    --manifest "$OUT/$variant/manifest.json"
done

python -m pip install --quiet zstandard
python - "$OUT" "$PRODUCT" "${VARIANTS[@]}" <<'PY'
import bz2, gzip, hashlib, json, lzma, os, subprocess, sys
import zstandard

out, product, variants = sys.argv[1], sys.argv[2], sys.argv[3:]
commit = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True,
                        text=True).stdout.strip()
record = {"commit": commit, "tauri_cli": "2.10.1", "variants": {}}
for v in variants:
    old = f"{out}/{v}/v1.0.0/{product}-setup.exe"
    new = f"{out}/{v}/v1.0.1/{product}-setup.exe"
    patch = os.path.getsize(f"{out}/{v}/patch.zst")
    full = os.path.getsize(new)
    row = {
        "installer_1_0_0": os.path.getsize(old),
        "installer_1_0_1": full,
        "installer_1_0_1_sha256": hashlib.sha256(open(new, "rb").read()).hexdigest(),
        "direct_patch": patch,
        "direct_patch_percent": round(patch / full * 100, 4),
    }
    if v == "none":
        data = open(new, "rb").read()
        row["full_transport_estimates"] = {
            "zstd_19": len(zstandard.ZstdCompressor(level=19).compress(data)),
            "xz_9e": len(lzma.compress(data, preset=9 | lzma.PRESET_EXTREME)),
            "gzip_9": len(gzip.compress(data, 9)),
            "bzip2_9": len(bz2.compress(data, 9)),
        }
    record["variants"][v] = row
json.dump(record, open(f"{out}/nsis-compression.json", "w"), indent=2)
print(json.dumps(record, indent=2))
print("::notice title=NSIS compression study::" + "; ".join(
    f"{v}: full {r['installer_1_0_1']} B, patch {r['direct_patch']} B "
    f"({r['direct_patch_percent']}%)" for v, r in record["variants"].items()))
PY
