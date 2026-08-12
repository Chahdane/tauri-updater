# Architecture

## The problem

Tauri's official updater downloads a complete installer artifact for every
update: an `.AppImage` on Linux, an NSIS `.exe` or `.msi` on Windows, an
`.app.tar.gz` on macOS. A release that changes one line of Rust still ships every
byte of the WebView assets, the bundled runtime, the icons and the vendored
libraries — all of which are already sitting on the user's disk, unchanged.

The waste is proportional to the app size and the release cadence. An app that
ships weekly at 80 MB moves roughly 4 GB per user per year, when the actual
change is usually a fraction of a percent of that.

## The core decision: wrap, don't replace

**This plugin does not implement updating. It implements *making the download
smaller*.**

The obvious design — patch the installed binary in place — is the one to avoid.
It runs straight into the hardest problems in the space:

- On Windows, a running `.exe` is locked; it cannot rewrite itself.
- On macOS, an app bundle is a signed directory tree; touching any byte inside it
  invalidates the code signature and the notarization ticket.
- Any in-place scheme has to be crash-safe against a power cut halfway through
  writing over the user's working application.

So instead of patching what is *installed*, this plugin patches what gets
*downloaded*. The target of the delta is the **installer artifact** — the exact
file the official updater would have fetched over HTTP.

The reconstructed file is byte-for-byte identical to the published artifact. That
identity is the whole trick. It means:

- Tauri's minisign signature over the artifact still validates.
- Tauri's per-platform install logic runs completely unchanged.
- Code signing, notarization and installer behaviour are untouched.
- If the delta path fails for any reason, falling back is trivial: download the
  file normally, exactly as before.

The diff/apply engine is therefore entirely platform-agnostic. It moves bytes
around and has no idea what an AppImage is. The only per-platform work is the
adapter that hands the finished artifact to the installer.

## The flow

```mermaid
flowchart TD
    A[App checks for update] --> B[Official updater reports<br/>version + artifact URL]
    B --> C{Patch manifest lists a patch<br/>from installed → target version?}
    C -- no --> F[Full download]
    C -- yes --> D[Download patch]
    D --> E[Apply patch to cached base artifact]
    E --> G{Rebuilt hash == manifest hash?}
    G -- no --> F
    G -- yes --> H[Hand artifact to Tauri's install step]
    F --> H
    H --> I[Tauri verifies signature and installs]

    D -. any error .-> F
    E -. any error .-> F
```

Four stages, in order:

1. **Diff** happens at release time, on CI, not on the user's machine. For each
   supported upgrade path (`1.2.0 → 1.3.0`, `1.2.1 → 1.3.0`, …) the release
   tooling produces a patch and records its size and the resulting artifact hash
   in a patch manifest.
2. **Apply** happens on the client: base artifact + patch → rebuilt artifact.
3. **Verify** hashes the rebuilt artifact and compares it against the hash in the
   manifest. This catches a corrupt patch, a corrupt base, a wrong base version
   and a bug in the backend, all with one check.
4. **Handoff** passes the verified file to the platform adapter, which gives it
   to the official updater's install step.

### Where the base artifact comes from

Applying a patch requires the *old* artifact, and an installed app does not
normally keep the installer it came from. Two strategies, both planned:

- **Cache the artifact** after an update completes, so the next update has its
  base ready. Cheap, but useless for the very first delta update and for anyone
  who installed from the website.
- **Reconstruct the base** from what is on disk where the installed layout allows
  it deterministically.

Until a base is available, the plugin simply does a full download. The design
never assumes the base is present.

## The `PatchBackend` trait

Delta algorithms have genuinely different trade-offs — patch size against memory
use against apply speed — and the right one depends on the artifact. The engine
is written against a trait so a backend can be swapped without touching the rest
of the pipeline:

```rust
pub trait PatchBackend: Send + Sync {
    /// Stable identifier recorded in the patch manifest, e.g. "zstd".
    fn id(&self) -> &'static str;

    /// Release-side: produce a patch that turns `old` into `new`.
    fn diff(&self, old: &Path, new: &Path, patch: &Path) -> Result<()>;

    /// Client-side: reconstruct `new` from `old` + `patch`.
    fn apply(&self, old: &Path, patch: &Path, out: &Path) -> Result<()>;
}
```

The manifest records which backend produced each patch, so a client can decline
a patch it cannot apply (and fall back) rather than guessing. That makes adding
a backend a backwards-compatible change.

**Backend 1 — zstd `--patch-from`.** The old artifact is referenced as a
compression *prefix*, with the window log raised to cover its full size, so the
compressor can copy directly from it. Mature, widely deployed, one dependency,
and fast to apply. This is the default.

**Backend 2 — bsdiff (planned).** Produces materially smaller patches for
recompiled binaries because it tolerates the small shifts a recompile causes, at
the cost of much higher memory use during diff. A candidate for release-time use
where CI memory is cheap.

## Per-platform adapter seams

Everything above is shared. Per-platform code is confined to one small trait: how
to obtain the base artifact, and how to hand the rebuilt one to the installer.

| Platform | Artifact | Notes |
| --- | --- | --- |
| Linux | `.AppImage` | Simplest case: a single self-contained file, no installer step, no code signature to preserve. This is why it is the first target — and why the engine can be proven on macOS, since round-tripping an AppImage is just file I/O. |
| Windows | NSIS `.exe` / `.msi` | Artifact is a single file. Handoff means pointing the existing installer-launch step at the rebuilt file. |
| macOS | `.app.tar.gz` | Compressed tarball, so unrelated changes perturb the compressed stream widely. Likely needs the delta taken against the *uncompressed* tar and the gzip layer reproduced deterministically — the hardest of the three, hence last. |

## Failure model

The safety property is: **the delta path can only ever make an update faster,
never make it fail.**

Every step from manifest lookup to hash verification is fallible and every
failure has exactly one recovery: discard whatever the delta path produced and
let the official updater do a full download. There is no error state in which the
user is left worse off than if this plugin were not installed.

`delta_core::Error` is `#[non_exhaustive]` and every variant is treated as
recoverable at the plugin boundary. Nothing in the delta path panics on
malformed or hostile input.

## Security notes

- **The hash check is a correctness gate, not the trust anchor.** Authenticity
  comes from the minisign signature over the target installer. A patch is
  untrusted input; it is only ever used to produce a candidate file that must
  then pass both checks.
- **The plugin performs the signature check itself, and must.** This was
  originally written assuming Tauri would verify whatever it was handed. It does
  not. In `tauri-plugin-updater` 2.10.1, `verify_signature` is called from
  exactly one place — inside `Update::download()` — and the handoff seam,
  `Update::install(bytes)`, goes straight to the installer with no verification.
  Since the delta path exists precisely to avoid `download()`, nothing would
  check the signature unless the plugin does. It therefore runs the check before
  every handoff, using Tauri's own verification code reproduced verbatim and
  pinned by `tauri_signature_compat.rs`. Skipping it would turn the delta path
  into a way to install unsigned artifacts, which is the one outcome this design
  exists to prevent.
- **Patches are treated as hostile.** Apply streams its output to disk and stops
  at a configurable ceiling, so a malicious patch cannot expand indefinitely or
  fill the user's disk.
- **Allocation is bounded before decompression starts.** The manifest declares
  the target installer's exact size, so the window zstd may allocate is derived
  from a number the release committed to in advance rather than from the patch's
  own header. A patch claiming a huge window log is rejected during frame setup.
  This closed the gap Phase 1 documented here.
- **No new network surface.** The plugin fetches from the update server the app
  already configures, and sends no telemetry.

## The manifest

Release-time output is a single `manifest.json` that is **a superset of Tauri's
own static updater document**. Tauri's `version`, `notes`, `pub_date` and
`platforms` fields are present and untouched; delta information lives under a
separate `delta` key.

That shape is deliberate. Publishing a second, delta-specific document would mean
two files that have to agree about URLs, versions and signatures while being
written by different code paths — and the drift shows up exactly when it hurts
most, in the fallback. One document cannot disagree with itself.

```
manifest.json
├── version, notes, pub_date          ── read by every Tauri client
├── platforms/<os-arch>/{url,signature}  ── the full-download fallback
└── delta/
    ├── schema, hash_algo
    └── platforms/<os-arch>/
        ├── target_version
        ├── target_installer_blake3   ── the reconstruction gate
        ├── target_installer_size     ── bounds allocation before decompressing
        ├── signature                 ── same value as the Tauri layer
        └── patches/<from_version>/{backend_id, patch_url, patch_blake3, patch_size}
```

Three properties worth stating explicitly:

- **The signature is over the target installer, never the patch.** Both paths end
  up holding the same artifact, so one signature validates both, and the delta
  path cannot install anything the full-download path would have refused.
- **Hashes name their algorithm.** Fields are `*_blake3` and `hash_algo` is
  explicit, so a client refuses a document it cannot check rather than comparing
  a BLAKE3 digest against a field that once held SHA-256.
- **Schema 1 patches only to the latest release.** One patch per update, never a
  chain. The `schema` field lets an older client decline a format it does not
  implement instead of misreading it.

## The release side

`delta-release` is file-in, file-out: previous installer plus new installer
produces the patch, the digests, the signature and the manifest. It runs on CI
once per release, never on a user's machine, and needs no network access — which
also makes it runnable by hand against two local files.

Because the engine and the tooling are tested against the identical fixtures, the
loop is closed in CI: a manifest generated by the real tool is handed to a
simulated client with nothing else, and the client has to reach a verified
artifact using only what the manifest recorded.

## What this plugin deliberately does not do

- Patch installed binaries in place.
- Replace, fork or vendor `tauri-plugin-updater`.
- Change how artifacts are signed, verified or installed.
- Require a particular hosting setup — the patch manifest is a static JSON file
  and can sit next to the existing update endpoint.
