# Roadmap

Phases are ordered so that each one is provable before the next begins. The
guiding constraint: development is happening on macOS with no Windows or Linux
machine available, so early phases are chosen to be fully verifiable here.

Platform order is **Linux (AppImage) → Windows → macOS end-to-end**, which is
deliberately not the development machine's own platform. AppImage is a single
self-contained file with no installer step and no code signature to preserve, so
the diff/apply engine can be proven against it with nothing but file I/O — which
runs anywhere, including here. macOS is last because `.app.tar.gz` is the hardest
artifact to delta well.

---

## Phase 1 — The engine

Prove that a patch can rebuild an artifact exactly, on this machine, offline.

- Workspace, project hygiene, docs, CI.
- `PatchBackend` trait; zstd as backend #1.
- Content hashing and verification.
- **Flagship test:** AppImage-shaped round-trip — two versions of a file, diff,
  apply, assert `hash(rebuilt) == hash(new)`. Must pass in CI.
- First real patch-size numbers for the README.

*Exit criteria:* round-trip test green in CI; patch measurably smaller than a full
artifact; corrupt and mismatched inputs fail loudly rather than silently.

## Phase 2 — Release tooling and CI

Make patches producible by a real release pipeline.

- Patch manifest format (versions, backend id, patch URL, sizes, artifact hash).
- CLI to generate patches and a manifest from a set of released artifacts.
- Retention policy — how many previous versions get a patch path.
- A reusable GitHub Actions workflow other projects can call.

*Exit criteria:* a tagged release produces patches and a valid manifest without
manual steps.

## Phase 3 — Client happy path

First end-to-end delta update, on Linux/AppImage.

- Tauri plugin crate wrapping `tauri-plugin-updater`.
- Manifest fetch, patch selection, download, apply, verify.
- Base-artifact caching after a successful update.
- Install handoff adapter for AppImage.
- Example app demonstrating the update.

*Exit criteria:* an AppImage app updates itself from a patch, verified in CI
containers.

## Phase 4 — Robustness and security

Make it safe to depend on.

- Full-download fallback wired into every failure path, with tests that force
  each failure.
- Bounded decompression; disk-space checks before writing.
- Resume and retry for interrupted patch downloads.
- Fuzzing the apply path against malformed patches.
- Concurrency and interrupted-update handling.

*Exit criteria:* every error variant has a test proving the update still
completes via full download.

## Phase 5 — Developer experience

Make it pleasant to adopt.

- TypeScript bindings and JS API.
- Progress events distinguishing patch download from full download.
- Configuration ergonomics and sensible defaults.
- Documented migration from plain `tauri-plugin-updater` — ideally two lines.

## Phase 6 — Windows

- NSIS/MSI artifact handling and install handoff.
- CI coverage on Windows runners.
- Needs a real Windows tester.

## Phase 7 — macOS end-to-end

- `.app.tar.gz` handling, likely deltaing the uncompressed tar with
  deterministic recompression.
- Verify code signature and notarization survive intact.

## Phase 8 — Ship

- crates.io and npm publication.
- Benchmarks across a few real-world app sizes.
- Upstream: report back on tauri-apps/tauri#11863.

---

## Deliberately out of scope

- Patching installed binaries in place.
- Forking or vendoring `tauri-plugin-updater`.
- Hosting patches — the manifest is a static file; bring your own storage.
- Telemetry of any kind.
