#!/usr/bin/env bash
# A size benchmark with a realistic payload, on macOS or Windows.
#
#   ./e2e/benchmark.sh [output-dir]
#
# Builds the example app three times with ~88 MiB of bundled resources, the
# shape of an app that ships media and data next to its executable:
#
#   1.0.0   32 x 2 MiB incompressible "media" files + 24 x 1 MiB text/JSON files
#   1.0.1   version change only                      (the smallest release)
#   1.0.2   2 media files replaced, 1 added, 5 text files edited (~2% of lines),
#           1 text file deleted, and a frontend change (a feature-sized release)
#
# and publishes 1.0.2 from both 1.0.1 and 1.0.0 with the real delta-release,
# measuring Full, direct-patch and (on macOS) tar-patch sizes for every pair.
# The payload is generated from a fixed seed, so every run patches the same
# bytes. Output: <output-dir>/benchmark.json.
#
# A measurement, not a release: --max-direct-patch-percent is 100 so every
# patch is kept and measured. Nothing is uploaded or published.

set -euo pipefail

OUT="${1:-$PWD/delta-benchmark}"
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$APP_DIR/../.." && pwd)"
KEY_PASSWORD="benchmark-password"
DATA="$APP_DIR/bench-data"

case "$(uname -s)" in
  Darwin)
    PLATFORM="darwin-$(uname -m | sed 's/arm64/aarch64/')"
    ARTIFACT_GLOB="$ROOT/target/release/bundle/macos/*.app.tar.gz"
    TAR_LAYER=1
    # delta-release recognises a tarball by its .tar.gz name, so keep it.
    ARTIFACT="artifact.app.tar.gz"
    DELTA_RELEASE="$ROOT/target/release/delta-release"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    PLATFORM="windows-x86_64"
    ARTIFACT_GLOB="$ROOT/target/release/bundle/nsis/*-setup.exe"
    TAR_LAYER=0
    ARTIFACT="artifact.exe"
    DELTA_RELEASE="$ROOT/target/release/delta-release.exe"
    ;;
  *) echo "FATAL: the benchmark builds macOS or Windows updater artifacts" >&2; exit 1 ;;
esac

# Windows runners name it python; macOS runners, python3.
PYTHON="$(command -v python3 || command -v python)"
[ "$(uname -s)" = Darwin ] || PYTHON="$(command -v python || command -v python3)"

EDITED=("$APP_DIR/tauri.conf.json" "$APP_DIR/Cargo.toml" "$APP_DIR/dist/index.html")
_LOCK_BACKUP="$(mktemp)"
cp "$ROOT/Cargo.lock" "$_LOCK_BACKUP"
cleanup() {
  git -C "$ROOT" checkout -- "${EDITED[@]}" 2>/dev/null || true
  rm -rf "$DATA" "$APP_DIR/dist/changelog.js" "$APP_DIR/bench.conf.json"
  [ -s "$_LOCK_BACKUP" ] && cp "$_LOCK_BACKUP" "$ROOT/Cargo.lock"
  rm -f "$_LOCK_BACKUP"
}
trap cleanup EXIT INT TERM

rm -rf "$OUT"; mkdir -p "$OUT"

cargo tauri signer generate --ci --password "$KEY_PASSWORD" \
  --write-keys "$OUT/key" --force >/dev/null 2>&1
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$OUT/key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD"

# Resources are layered on with --config, so the committed config is untouched.
printf '{ "bundle": { "resources": ["bench-data/*"] } }\n' > "$APP_DIR/bench.conf.json"

cargo build -q --release -p tauri-updater-delta-release --manifest-path "$ROOT/Cargo.toml"

prepare() {
  local version="$1"
  git -C "$ROOT" checkout -- "${EDITED[@]}"
  rm -f "$APP_DIR/dist/changelog.js"
  "$PYTHON" - "$APP_DIR" "$DATA" "$version" "$OUT/key.pub" <<'PY'
import json, pathlib, random, re, shutil, sys
app, data, version, pub = sys.argv[1:5]
app, data = pathlib.Path(app), pathlib.Path(data)

conf = app / "tauri.conf.json"
cfg = json.loads(conf.read_text())
cfg["version"] = version
cfg["plugins"]["updater"]["pubkey"] = pathlib.Path(pub).read_text().strip()
conf.write_text(json.dumps(cfg, indent=2) + "\n")
cargo = app / "Cargo.toml"
cargo.write_text(re.sub(r'^version = "[^"]+"$', f'version = "{version}"',
                        cargo.read_text(), count=1, flags=re.M))

def media(seed, size=2 * 1024 * 1024):
    return random.Random(seed).randbytes(size)

def text(seed, edited=0.0):
    rng = random.Random(seed)
    edit = random.Random(seed + 1_000_000)
    lines, total = [], 0
    while total < 1024 * 1024:
        i = len(lines)
        row = {"id": i, "name": f"item-{rng.randrange(10**6)}",
               "score": round(rng.random(), 6), "tags": [f"t{rng.randrange(50)}" for _ in range(3)]}
        if edited and edit.random() < edited:
            row["score"] = round(edit.random(), 6)
        line = json.dumps(row) + "\n"
        lines.append(line)
        total += len(line)
    return "".join(lines).encode()

shutil.rmtree(data, ignore_errors=True)
data.mkdir(parents=True)
feature = version == "1.0.2"
for i in range(32):
    seed = i + (10_000 if feature and i in (3, 17) else 0)  # re-encoded media
    (data / f"media-{i:02}.bin").write_bytes(media(seed))
if feature:
    (data / "media-32.bin").write_bytes(media(32))           # a new asset
for i in range(24):
    if feature and i == 11:
        continue                                              # a removed file
    edited = 0.02 if feature and i in (2, 5, 8, 13, 21) else 0.0
    (data / f"data-{i:02}.json").write_bytes(text(i, edited))

if feature:
    index = app / "dist" / "index.html"
    index.write_text(index.read_text() + '<p>What is new.</p>\n<script src="changelog.js"></script>\n')
    (app / "dist" / "changelog.js").write_text(
        "\n".join(f"// entry {i}" for i in range(1600)) + "\n")
PY
}

build() {
  local version="$1" dest="$OUT/v$1"
  echo "==> building $version"
  prepare "$version"
  mkdir -p "$dest"
  cp "$APP_DIR/tauri.conf.json" "$APP_DIR/Cargo.toml" "$dest/"
  rm -rf "$ROOT/target/release/bundle"
  ( cd "$APP_DIR" && cargo tauri build --config bench.conf.json )
  shopt -s nullglob
  local found=($ARTIFACT_GLOB)
  [ "${#found[@]}" -eq 1 ] || { echo "FATAL: expected one artifact, found ${#found[@]}" >&2; exit 1; }
  cp "${found[0]}" "$dest/$ARTIFACT"
}

build 1.0.0
build 1.0.1
build 1.0.2

publish() {
  local to="$1"; shift
  local args=(
    --platform "$PLATFORM"
    --app-config "$OUT/v$to/tauri.conf.json"
    --target-version "$to"
    --new-installer "$OUT/v$to/$ARTIFACT"
    --installer-url "https://example.invalid/v$to/$ARTIFACT"
    --max-direct-patch-percent 100
    --manifest "$OUT/manifest-$to.json"
  )
  for from in "$@"; do
    args+=(
      --from-version "$from"
      --previous-installer "$OUT/v$from/$ARTIFACT"
      --patch-url "https://example.invalid/$from-$to.zst"
      --patch-out "$OUT/$from-$to.zst"
    )
    if [ "$TAR_LAYER" = 1 ]; then
      args+=(--tar-patch-url "https://example.invalid/$from-$to.tar.zst"
             --tar-patch-out "$OUT/$from-$to.tar.zst")
    fi
  done
  [ "$TAR_LAYER" = 1 ] && args+=(--require-tar-layer)
  "$DELTA_RELEASE" "${args[@]}"
}

publish 1.0.1 1.0.0
publish 1.0.2 1.0.1 1.0.0

"$PYTHON" - "$OUT" "$PLATFORM" "$ARTIFACT" <<'PY'
import json, os, subprocess, sys
out, platform, artifact = sys.argv[1], sys.argv[2], sys.argv[3]
size = lambda p: os.path.getsize(p) if os.path.exists(p) else None
record = {
    "platform": platform,
    "commit": subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip(),
    "payload": "32x2MiB random + 24x1MiB JSON lines; see e2e/benchmark.sh",
    "artifacts": {v: size(f"{out}/v{v}/{artifact}") for v in ("1.0.0", "1.0.1", "1.0.2")},
    "pairs": {},
}
for frm, to, kind in [("1.0.0", "1.0.1", "version only"),
                      ("1.0.1", "1.0.2", "feature change"),
                      ("1.0.0", "1.0.2", "two releases behind")]:
    full = record["artifacts"][to]
    direct, tar = size(f"{out}/{frm}-{to}.zst"), size(f"{out}/{frm}-{to}.tar.zst")
    pct = lambda n: None if n is None else round(n / full * 100, 3)
    best = min(n for n in (direct, tar) if n is not None)
    record["pairs"][f"{frm}->{to}"] = {
        "kind": kind, "full": full,
        "direct_patch": direct, "direct_percent": pct(direct),
        "tar_patch": tar, "tar_percent": pct(tar),
        "best_download": best, "best_percent": pct(best),
    }
    if os.environ.get("DELTA_BENCH_BSDIFF") == "1":
        # Research only: what an executable-aware diff would cost on the same
        # bytes the shipped backend patches (the tar on macOS, the installer on
        # Windows). Not a backend this project ships.
        import bsdiff4, gzip
        def payload(v):
            data = open(f"{out}/v{v}/{artifact}", "rb").read()
            return gzip.decompress(data) if tar is not None else data
        started = __import__("time").time()
        bs = len(bsdiff4.diff(payload(frm), payload(to)))
        record["pairs"][f"{frm}->{to}"].update({
            "bsdiff_patch": bs, "bsdiff_percent": pct(bs),
            "bsdiff_seconds": round(__import__("time").time() - started, 1),
        })
json.dump(record, open(f"{out}/benchmark.json", "w"), indent=2)
print(json.dumps(record, indent=2))
print(f"::notice title=Delta benchmark ({platform})::" + "; ".join(
    f"{k} ({p['kind']}): best {p['best_percent']}% of {p['full']} B"
    for k, p in record["pairs"].items()))
PY
