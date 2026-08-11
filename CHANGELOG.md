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
- Project documentation: architecture, roadmap, sprint tracking and contribution
  guide.

[Unreleased]: https://github.com/Chahdane/tauri-updater/commits/main
