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
| Release-side patch generation tooling | Planned |
| Tauri plugin + install handoff | Planned |

The [roadmap](docs/ROADMAP.md) has the phase-by-phase plan, and
[SPRINT.md](SPRINT.md) tracks what is being worked on right now.

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
release process published, Tauri's own signature verification and installer
logic run unchanged. Nothing about how the app installs itself is modified —
which is what avoids the locked-executable and self-overwrite problems that make
in-place binary patching so painful on Windows and macOS.

Two consequences worth stating plainly:

- **A bad patch cannot break an install.** Every failure on the delta path —
  missing patch, corrupt download, hash mismatch, unknown backend, out of disk —
  falls back to the full download the official updater would have done anyway.
  The delta path is an optimisation, never a dependency.
- **The delta path does not weaken signature checking.** The reconstructed
  artifact still has to satisfy Tauri's minisign verification. The hash check
  this plugin performs is an additional correctness gate, not a replacement.

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
apps will land here as Phase 2 produces them.

## Installation

Not published to crates.io or npm yet. Once it is, this section will carry the
real install steps.

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
