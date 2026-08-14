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

## Phase 1 — The engine ✅

Prove that a patch can rebuild an artifact exactly, on this machine, offline.

- Workspace, project hygiene, docs, CI.
- `PatchBackend` trait; zstd as backend #1.
- Content hashing and verification.
- **Flagship test:** AppImage-shaped round-trip — two versions of a file, diff,
  apply, assert `hash(rebuilt) == hash(new)`. Must pass in CI.
- First real patch-size numbers for the README.

*Exit criteria:* round-trip test green in CI; patch measurably smaller than a full
artifact; corrupt and mismatched inputs fail loudly rather than silently.

*Delivered:* 6,815,744 byte artifact → 393,782 byte patch (5.78%), reproduced
byte-identically on Linux, macOS and Windows and asserted in CI.

## Phase 2 — Release tooling and CI ✅

Make patches producible by a real release pipeline.

- Patch manifest format (versions, backend id, patch URL, sizes, artifact hash).
- CLI to generate patches and a manifest from a set of released artifacts.
- Retention policy — how many previous versions get a patch path.
- A reusable GitHub Actions workflow other projects can call.

*Exit criteria:* a tagged release produces patches and a valid manifest without
manual steps.

*Delivered:* `delta-release` (patch + digests + signature + manifest),
the Tauri-superset manifest, a dry-runnable `release.yml`, and an end-to-end test
where a client reaches a verified artifact from the generated manifest alone.
Wiring the workflow to real bundler output waits for the Phase 3 example app.

## Phase 3 — Client happy path — mostly built, exit criteria NOT met

- [x] Tauri plugin crate wrapping `tauri-plugin-updater`.
- [x] Patch selection, download, apply, verify. **Manifest fetch was removed
      rather than built**: the release document now comes from Tauri's own check
      (DECISIONS #13), which is a stronger position than the one planned here.
- [ ] Base-artifact caching after a successful update. Still supplied by the
      harness rather than by the plugin.
- [x] Install handoff adapter, via `TauriInstall` over `Update::install`.
- [x] Example app, in `examples/desktop-app`.

*Exit criteria:* **not met.** An AppImage app updating itself from a patch has
never been observed. Every test to date uses a recording handoff by design, so
nothing green so far substitutes for it. This is the project's single largest
open claim.

## Phase 4 — Robustness and security — largely delivered by the hardening sprint

Most of this was brought forward by the post-Codex audit; the resulting
decisions are in `DECISIONS.md` and the evidence in `../research/`.

- [x] Full-download fallback wired into every failure path, with tests forcing
      each failure.
- [x] Bounded decompression, and a **local** ceiling on the declared target size
      that the update server cannot influence (DECISIONS #19).
- [x] Concurrency: one workspace per update (DECISIONS #18).
- [x] Interrupted-update handling: downloads and reconstructions build into
      `.part` files promoted by rename only after passing their checks.
- [x] Transport bounds: HTTPS policy, redirect budget, request deadline,
      response-size caps.
- [x] Update identity, downgrade and replay policy (DECISIONS #13, #14) — not
      originally planned for this phase, and the most important thing in it.
- [ ] Resume and retry for interrupted patch downloads. Not built; a failed
      patch download falls back to a full download instead.
- [ ] Fuzzing the apply path against malformed patches. Assessed and deferred —
      see the research ledger for why ordinary unit tests are not a substitute.
- [ ] Disk-space checks before writing.

*Exit criteria:* partially met. Every error variant has a test proving the update
still completes via full download, **except** the two that deliberately refuse
rather than fall back: a signature failure (#11) and a version-policy or identity
violation (#14).

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
