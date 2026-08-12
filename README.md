# tauri-plugin-updater-delta

Binary delta (differential) updates for Tauri v2 — ship a small patch instead of
a full installer.

A Tauri app that adds a fixed typo to a label still makes every user re-download
the entire bundle. For a mid-sized desktop app that is 40–120 MB per release, per
user. Most of those bytes are byte-identical to the ones already on disk. This
plugin downloads only the difference and rebuilds the installer locally.

Implements the request in [tauri-apps/tauri#11863](https://github.com/tauri-apps/tauri/issues/11863).

---

## Status

**Pre-alpha. Under active development — not usable in an app yet.**

| Piece | State |
| --- | --- |
| Content hashing + verification (`delta-core`) | Working, tested |
| `PatchBackend` trait + zstd backend | Working, tested |
| Release manifest (Tauri superset) | Working, tested |
| `delta-release` patch + manifest tool | Working, tested |
| Tauri plugin + install handoff | Planned |

The [roadmap](docs/ROADMAP.md) has the phase-by-phase plan,
[SPRINT.md](SPRINT.md) tracks what is being worked on right now, and
[docs/DECISIONS.md](docs/DECISIONS.md) records the non-obvious calls and why they
were made.

## How it works

It **wraps** the official `tauri-plugin-updater` rather than replacing it.

The official updater's job is to fetch an installer artifact and then run it.
This plugin slips in between those two steps: it reconstructs that exact same
artifact locally from a patch, verifies it byte-for-byte, and hands the finished
file to Tauri's existing install path.

```
official updater:   check → download full artifact ────────────→ install
this plugin:        check → download patch → apply → verify ───→ install
                                    └── on any failure ─────────┘
                                        (full download, as normal)
```

Because the artifact handed to the installer is bit-identical to the one the
release process published, Tauri's own installer logic runs unchanged, and the
same signature validates it. Nothing about how the app installs itself is
modified —
which is what avoids the locked-executable and self-overwrite problems that make
in-place binary patching so painful on Windows and macOS.

Two consequences worth stating plainly:

- **A bad patch cannot break an install.** Every failure on the delta path —
  missing patch, corrupt download, hash mismatch, unknown backend, out of disk —
  falls back to the full download the official updater would have done anyway.
  The delta path is an optimisation, never a dependency.
- **The signature is still checked — by this plugin, explicitly.** Tauri
  verifies signatures inside `Update::download()`, which the delta path bypasses
  by definition: not downloading the full artifact is the entire point. The
  handoff seam, `Update::install()`, performs *no* verification. So this plugin
  runs the check itself before handing anything to the installer, using code
  copied verbatim from `tauri-plugin-updater` and pinned by a test. Same
  signature, same algorithm, same key — performed by us rather than by them,
  because there is no code path in which Tauri would do it for us.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full design.

## Benchmarks

From the round-trip test in this repository — reproduce with
`cargo test -p tauri-updater-delta-core -- --nocapture`:

| | Bytes | |
| --- | ---: | --- |
| Full artifact | 6,815,744 | 6.50 MiB |
| Patch | 393,782 | 0.38 MiB |
| **Download** | | **5.78% — 17.3× smaller** |

Read these honestly. The fixture is a *synthetic* AppImage, not a real app: an
ELF stub plus high-entropy payload, where a release rewrites the 256 KiB app
binary and inserts a 128 KiB asset. That makes 393,216 bytes of it genuinely new.

The patch came to 393,782 bytes — **566 bytes of overhead over the theoretical
minimum**. So the number to take from this is not "17× smaller"; it is that the
engine reuses effectively everything reusable, including across an insertion that
shifts every following byte. The ratio you actually get is set by how much your
release changes, not by the engine.

The fixture is deliberately incompressible, which is what makes the result
meaningful: with a compressible fixture zstd would post a good number by simply
compressing the artifact, proving nothing about deltas. Numbers from real Tauri
apps will land here in Phase 3, once there is an example app producing real
bundles to measure.

These bytes are reproduced identically on Linux, macOS and Windows — asserted in
CI rather than compared by eye, since `zstd-sys` compiles a vendored C library
and MSVC is the likeliest place a divergence would hide.

### Ratios differ enormously by platform — read this before adopting

| Platform | Artifact | Measured | Status |
| --- | --- | ---: | --- |
| **Linux** | `.AppImage` | **5.78%** | Works as intended |
| **macOS** | `.app.tar.gz` | **95.14%** | Correct, but saves almost nothing yet |
| **Windows** | NSIS `.exe` / `.msi` | not measured | Unknown |

macOS is measured on a real bundle built by `cargo tauri build`, not a fixture,
and the result is honest: **a delta currently saves about 5% of a macOS
download.** The artifact is gzip-compressed, and gzip is a streaming format, so
changing a few bytes of the main binary shifts every compressed byte after it.
The two artifacts share almost nothing *as presented*, even though the
uncompressed tars are nearly identical.

The fix — delta the uncompressed tar and reproduce the gzip layer
deterministically — is designed but not built. Until it lands, enable this on
Linux and treat macOS as correct-but-not-yet-worthwhile. Windows uses compressed
containers too, so expect it to look more like macOS than Linux until measured.

Reasoning and the route out are in [docs/DECISIONS.md](docs/DECISIONS.md) #15.
A single averaged "delta updates for Tauri" number would overpromise on two
platforms out of three, so this table exists instead.

## Installation

Not published to crates.io or npm yet. Once it is, this section will carry the
real install steps.

## Producing a release

`delta-release` turns two installers into a patch and a manifest. It is
file-in/file-out and needs no network access, so it runs by hand exactly as it
runs on CI:

```sh
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/myapp.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="…"

delta-release \
  --platform linux-x86_64 \
  --version 1.0.1 \
  --from-version 1.0.0 \
  --previous-installer dist/app_1.0.0.AppImage \
  --new-installer     dist/app_1.0.1.AppImage \
  --installer-url https://releases.example.com/app_1.0.1.AppImage \
  --patch-url     https://releases.example.com/1.0.0-to-1.0.1.zst \
  --patch-out     dist/1.0.0-to-1.0.1.zst \
  --manifest      dist/manifest.json
```

The `manifest.json` it writes **is** a valid Tauri static updater document — the
same `version`, `pub_date` and `platforms` fields the official updater already
reads, with delta information added alongside. Point Tauri's updater at it and a
delta-unaware client behaves exactly as before.

The signature it records is over the **target installer**, not the patch, so the
same signature validates whether a user rebuilt the artifact from a patch or
downloaded it whole. Run the tool once per upgrade path you want to support;
repeated runs against the same version add to the manifest rather than replacing
it. Pass `--dry-run` to see the manifest without writing it.

## Configuration

The intended API, for orientation. **This does not exist yet** — it is recorded
here so the design can be argued with before it is built.

```rust
fn main() {
    tauri::Builder::default()
        // The official updater is still the one that installs.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // This plugin makes its download smaller when it can.
        .plugin(
            tauri_plugin_updater_delta::Builder::new()
                .patch_manifest_url(
                    "https://releases.example.com/{{target}}/{{arch}}/patches.json",
                )
                .build(),
        )
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

## Privacy

No telemetry, no analytics, no phone-home. The plugin talks to the update server
you configure and to nothing else. Patch application is entirely local and works
offline once the patch is downloaded.

## Unsigned builds

Anything produced from this repository is an **unsigned build**. It is not code-signed
for macOS or Windows, and it is not notarized by Apple. Gatekeeper and SmartScreen
will warn about it, and you should treat any binary from here as development
software rather than something to ship to end users.

This is separate from update signing: your own app's release artifacts should
still be signed with your Tauri minisign key, and this plugin is designed so that
signature check keeps working normally.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Issues and PRs welcome, especially from
anyone who can test on Windows or Linux — development is currently happening on
macOS only.

## License

MIT — see [LICENSE](LICENSE).
