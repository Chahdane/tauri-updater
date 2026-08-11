# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Corrected the declared minimum supported Rust version from 1.77 to 1.85. The
  1.77 claim was wrong and CI's `msrv` job failed on it: `blake3` pulls in
  edition-2024 crates, which no toolchain before 1.85 can parse. Reasoning
  recorded in `docs/DECISIONS.md`.
- The AppImage round-trip test can no longer pass vacuously. Nothing asserted
  that the two fixture versions actually differ, so a generator change making
  them identical would have left the whole suite green while proving nothing.

### Added

- `docs/DECISIONS.md` — log of non-obvious decisions and the conditions that
  would cause them to be revisited.

- Cargo workspace with the `tauri-updater-delta-core` crate, holding the
  platform-agnostic diff/apply engine.
- `FileHash` and `verify_file` — BLAKE3 content hashing used to prove a
  reconstructed artifact matches the published one byte-for-byte.
- `PatchBackend` trait, and `backend_for` to resolve a backend from the
  identifier recorded in a patch manifest.
- `ZstdBackend` — the default backend, using zstd prefix-referencing (the
  library equivalent of `zstd --patch-from`). Applying streams its output to
  disk and stops at a configurable ceiling, so a hostile patch cannot force an
  unbounded write.
- AppImage round-trip test suite covering exact reconstruction plus corrupt,
  truncated, wrong-base and oversized-output patches. On the synthetic fixture
  a patch is 5.78% of a full download.
- Project documentation: architecture, roadmap, sprint tracking and contribution
  guide.

[Unreleased]: https://github.com/Chahdane/tauri-updater/commits/main
