# Roadmap

The current v0.1 target is the demonstrated macOS `.app.tar.gz` path. Engine
tests run on all three desktop CI platforms, but that is not a Linux or Windows
client-support claim.

## Foundation — complete

- Platform-independent BLAKE3 verification and zstd patch backend.
- Tauri-superset static update manifest.
- Release CLI, patch generation, signature generation, and publish checks.
- One authoritative Tauri updater check and the real Tauri install handoff.
- Persistent content-addressed cache and exact macOS tar recompression.
- Real macOS `1.0.0 → Full → 1.0.1 → TarDelta → 1.0.2` evidence.

## Gate P1 — release identity security — merged

- Authenticated `delta-v1` release identity in minisign's trusted comment.
- Legacy signatures remain Full-only.
- Authenticated contradictions fail closed.
- Opaque update session binds identity and installation to the same Tauri
  `Update`.

## Gate P2 — runtime hardening — merged

- Per-transaction workspaces and abandoned-workspace collection.
- Local resource ceilings for every tar stage.
- Cache corruption handling, generation compare-and-set, and immutable blob
  publication.
- PENDING/ACTIVE launch reconciliation and exact verified install handoff.

## Gate P3 — release correctness — merged

- First and unusable-predecessor releases produce valid signed Full-only updater
  documents.
- Direct and tar patches self-round-trip before publication.
- The workflow targets the demonstrated macOS artifact and pins Tauri CLI
  2.10.1.

## Gate P4 — developer experience/public API — under review

- Small Rust-first plugin API with managed updater state.
- Safe path, cache, transport, and limit defaults.
- Explicit Full, DirectDelta, and TarDelta outcomes.
- Non-fatal cache diagnostics and coarse progress phases.
- Normal example separated from the feature-gated localhost E2E harness.
- Compiling quickstart and fresh-clone onboarding rehearsal.
- Real macOS regression through the same public API applications use.

No full JS/TS SDK is planned for v0.1. A frontend can trigger one ordinary Rust
Tauri command; adding a second updater API would increase maintenance and attack
surface without improving the security boundary.

## Remaining before v0.1

- Gate P5/final independent security and release-readiness audit.
- Decide how the two credential-bound validation gaps affect release readiness:
  real GitHub-hosted HTTPS Full→TarDelta and Apple Developer ID/notarized E2E.
- Package metadata, publication rehearsal, and crates.io release.

P4 does not perform any of these steps.

## After macOS v0.1

- Linux client integration and real AppImage install evidence.
- Windows artifact handling and real NSIS/MSI install evidence.
- Resume/retry policy and disk-space preflight if evidence justifies them.
- Additional patch backends only when measured artifacts justify their cost.
- A JS/TS wrapper only if real application integrations show Rust commands are
  insufficient.

## Deliberately out of scope

- Patching installed binaries in place.
- Forking or vendoring `tauri-plugin-updater`.
- Hosting update assets.
- Telemetry.
- Claiming manifest freshness or TUF-style metadata security.
