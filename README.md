# tauri-plugin-updater-delta

Differential updates for Tauri v2. The plugin keeps Tauri's official update
check and installer, but may reconstruct the exact published artifact from a
smaller patch before handing it to `tauri-plugin-updater`.

This is pre-release software. The supported v0.1 path is **macOS
`.app.tar.gz`**. Linux and Windows client support is not claimed, and the final
security audit and release gate have not happened yet.

## What is proven

- A real macOS app completed `1.0.0 → 1.0.1` via Full, relaunched with its
  verified artifact ACTIVE, then completed `1.0.1 → 1.0.2` via TarDelta through
  the real Tauri installer.
- The harness asserts the update source and exact installed binary hashes, so a
  Full fallback cannot masquerade as a delta success.
- On controlled example-app pairs, a direct compressed-artifact patch was about
  95–97% of Full while a tar-layer patch was about 15%. These measurements are
  evidence about those builds, not universal macOS ratios.
- Final artifact signature verification, authenticated release identity,
  bounded reconstruction, cache re-verification, and release-time patch
  round-trips are enforced and tested.

GitHub-hosted HTTPS Full→TarDelta and Apple Developer ID/notarized end-to-end
tests remain credential-bound validation gaps. See
[Releasing](docs/RELEASING.md) and the evidence ledger in
[research/FINDINGS.md](research/FINDINGS.md).

## Quickstart

### 1. Prerequisites

- Rust 1.88 or newer.
- A Tauri v2 macOS application already using updater artifacts.
- `tauri-plugin-updater` 2.10.1. The delta plugin deliberately supports
  `>=2.10.1, <2.11.0` because its security assumptions were verified against
  that upstream implementation.
- An HTTPS location for `manifest.json`, the full `.app.tar.gz`, and patches.
- A Tauri updater signing key. Keep the private key out of source control.

The plugin and release tool are not published yet. Add them from Git while v0.1
is under review:

```sh
cargo add tauri-plugin-updater@=2.10.1
cargo add tauri-plugin-updater-delta \
  --git https://github.com/Chahdane/tauri-updater.git
```

### 2. Register both plugins

```rust
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_updater_delta::Builder::new().build())
        .run(tauri::generate_context!())
        .expect("error while running the Tauri application");
}
```

`Builder::new()` is the normal configuration. It derives the app identifier,
updater public key, platform, architecture, cache namespace, cache location,
transaction location, transport policy, and local safety ceilings.

### 3. Configure Tauri's updater

The existing Tauri updater configuration remains authoritative:

```json
{
  "bundle": {
    "createUpdaterArtifacts": true
  },
  "plugins": {
    "updater": {
      "endpoints": [
        "https://releases.example.com/{{target}}/{{arch}}/manifest.json"
      ],
      "pubkey": "YOUR_BASE64_TAURI_MINISIGN_PUBLIC_KEY"
    }
  }
}
```

The public key must be present in `tauri.conf.json`; the delta plugin reads the
same configured value. There is no delta-specific manifest URL and no second
manifest request.

### 4. Check and install

```rust
use tauri_plugin_updater_delta::{DeltaUpdaterExt, Outcome};

async fn check_for_updates(app: tauri::AppHandle) -> Result<String, String> {
    let Some(update) = app
        .delta_updater()
        .check()
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok("up-to-date".to_owned());
    };

    let outcome = update
        .install()
        .await
        .map_err(|error| error.to_string())?;

    for diagnostic in outcome.diagnostics() {
        // Installation succeeded. This warning means a future update may need
        // Full again because the cache was unavailable or could not be written.
        log::warn!("{diagnostic}");
    }

    Ok(match outcome {
        Outcome::InstalledFromFullDownload { .. } => "installed-full",
        Outcome::InstalledFromDirectDelta { .. } => "installed-direct-delta",
        Outcome::InstalledFromTarDelta { .. } => "installed-tar-delta",
        Outcome::UpToDate { .. } => "up-to-date",
        _ => "installed",
    }
    .to_owned())
}
```

For a frontend button, expose that Rust function as a normal `#[tauri::command]`
and invoke it from the frontend. v0.1 is intentionally Rust-first; it does not
add a second JS/TS updater SDK or expose identity, cache, and verification
internals to frontend code. The complete version is
[examples/desktop-app/src/update.rs](examples/desktop-app/src/update.rs).

`check_with` and `check_and_install_with` accept a callback for truthful coarse
phases: Checking, Downloading, Reconstructing, Verifying, Installing, and
Finished. Percentages are not invented where the underlying work cannot report
them accurately. An `Err` is the failure signal.

### 5. Produce release artifacts

Install the repository's release tool, or run the same package from a checkout:

```sh
cargo install --locked --git https://github.com/Chahdane/tauri-updater.git \
  --package tauri-updater-delta-release
```

Use the same private key Tauri uses:

```sh
export TAURI_SIGNING_PRIVATE_KEY="$(cat /secure/path/my-app.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="your-real-password"
```

For the first published updater release, omit predecessor flags. This produces a
valid signed Full-only `manifest.json`:

```sh
delta-release \
  --app-id com.example.myapp \
  --platform darwin-aarch64 \
  --target-version 1.0.1 \
  --new-installer dist/MyApp.app.tar.gz \
  --installer-url https://releases.example.com/v1.0.1/MyApp.app.tar.gz \
  --signature-out dist/MyApp.app.tar.gz.sig \
  --manifest dist/manifest.json
```

For a later release with a usable predecessor, add both direct and tar-layer
outputs:

```sh
delta-release \
  --app-id com.example.myapp \
  --platform darwin-aarch64 \
  --target-version 1.0.2 \
  --from-version 1.0.1 \
  --previous-installer dist/MyApp-1.0.1.app.tar.gz \
  --new-installer dist/MyApp.app.tar.gz \
  --installer-url https://releases.example.com/v1.0.2/MyApp.app.tar.gz \
  --patch-url https://releases.example.com/v1.0.2/1.0.1-to-1.0.2.zst \
  --patch-out dist/1.0.1-to-1.0.2.zst \
  --tar-patch-url https://releases.example.com/v1.0.2/1.0.1-to-1.0.2.tar.zst \
  --tar-patch-out dist/1.0.1-to-1.0.2.tar.zst \
  --require-tar-layer \
  --signature-out dist/MyApp.app.tar.gz.sig \
  --manifest dist/manifest.json
```

Use `darwin-x86_64` for an Intel build. `--app-id`, platform, and target version
are explicit because they enter the cryptographically authenticated release
identity; guessing them would hide a real security decision. The release tool
derives artifact digest and size itself and applies every generated patch before
publishing its metadata.

The asset set and pre-upload verification command are documented in
[docs/RELEASING.md](docs/RELEASING.md).

## First update and fallback behavior

A fresh installation has no official updater artifact cached:

```text
first update after adoption or cache miss -> Full -> stage PENDING
updated app launches                    -> promote to ACTIVE
later compatible update                 -> TarDelta when published and valid
```

Differential updates are an optimization, not a promise for every release. A
missing/corrupt cache, missing patch, download failure, unsupported patch, or
reconstruction mismatch safely degrades to Full. Cache persistence failures are
returned as non-fatal `Diagnostic` values on the successful `Outcome`; they do
not turn a correct install into a failure.

Authenticated release-identity contradictions, downgrades, and final signature
failures are different: they fail closed and install nothing. Legacy signatures
without a `delta-v1` identity remain Full-compatible but cannot use DirectDelta
or TarDelta.

## Advanced configuration

Most apps need none. `Builder` permits explicit overrides for cache/work paths,
download limits, timeouts, redirect count, reconstruction `Limits`, and
`CacheLimits`. Server metadata can never raise those local ceilings.

Plain HTTP and explicit endpoint/base overrides exist only under the non-default
`test-support` feature used by this repository's E2E harness. They are absent
from a normal build.

## Compatibility and security scope

- The plugin always installs through the exact Tauri `Update` returned by the
  one authoritative check. Applications cannot construct that binding or a
  verified install handoff.
- Cache contents are untrusted: kind, size, digest, and signature are checked on
  reuse. A successful install stages PENDING; only observing that version on a
  later launch promotes it to ACTIVE.
- The release identity format and security rationale live in
  [docs/DECISIONS.md](docs/DECISIONS.md). Implementation flow and resource
  boundaries live in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
- The manifest itself is not authenticated and release freshness is not proven.
  This is not a TUF-style update framework.
- No telemetry is added. The plugin contacts only the updater and artifact URLs
  selected from the application's existing updater response.

## Project documentation

- [Releasing](docs/RELEASING.md): publish and verify update assets.
- [Architecture](docs/ARCHITECTURE.md): internal data flow and safety boundaries.
- [Decisions](docs/DECISIONS.md): why non-obvious choices were made.
- [Research](research/): measurements and claim evidence.
- [Contributing](CONTRIBUTING.md): build, test, and contribution workflow.

## License

MIT — see [LICENSE](LICENSE).
