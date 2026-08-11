# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
