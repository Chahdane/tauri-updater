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

- `TauriInstall` — hands a verified artifact to `tauri_plugin_updater::Update`,
  and the plugin registration/`Builder`. All Tauri-specific code lives in one
  thin module; the flow beneath it has no Tauri dependency.
- `tauri-plugin-updater-delta` — the client flow: fetch manifest, select a patch,
  download it, reconstruct, verify, hand off. Both boundaries are traits
  (`Fetch`, `InstallHandoff`), so the whole path is tested offline with no Tauri
  runtime. `InstallHandoff::install` takes a `VerifiedArtifact`, so an unverified
  install cannot be expressed.
- `VerifiedArtifact` — a token obtainable only through successful minisign
  verification, so the install handoff cannot be reached with unverified bytes.
  It owns the verified bytes rather than a path, closing the window in which a
  file could be swapped between the check and the handoff.
- `client::plan_update` and the `Fetch` trait — the whole client decision path,
  testable with canned responses and with no Tauri or network dependency.
- `delta-release` — release-time tool that turns the previous and new installers
  into a patch, digests, a minisign signature and a `manifest.json`. File-in,
  file-out, no network access.
- Release manifest as a **superset of Tauri's static updater JSON**: the same
  `version`, `notes`, `pub_date` and `platforms` fields, with delta information
  under a separate `delta` key. The full-download fallback is therefore the
  official updater's own document, so the two cannot drift apart.
- `try_reconstruct` — the delta path's entry point. It returns
  `Reconstruction::Verified` or `Reconstruction::FallBack` and cannot fail, so
  no error can escape the delta path and abort an install.
- `ZstdBackend::with_expected_output_bytes` — bounds both the output and the
  zstd window from the size the manifest declares, closing the allocation gap
  documented in Phase 1.
- End-to-end test suite: a manifest produced by the real release tool is handed
  to a simulated client with nothing else, which must reach a verified artifact
  from the manifest alone. Corrupt, truncated, wrong-base, unknown-backend and
  wrong-signing-key cases each assert a fall back to full download.
- `release.yml` — tagged-release workflow, dry-runnable via `workflow_dispatch`
  so the wiring can be exercised without cutting a release.
- Shared fixtures crate, so the engine and the release tooling are tested
  against byte-identical artifacts.
- `patch_bytes_are_identical_on_every_platform` — pins the patch digest so
  cross-platform determinism is enforced by CI rather than eyeballed once.
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

### Gate B — transport and resource safety

- **HTTPS required.** Non-HTTPS URLs are refused in release builds and warned
  about in development, matching `tauri-plugin-updater`'s own policy and its
  `dangerousInsecureTransportProtocol` opt-in — applied to every URL, not only
  to manifest endpoints.
- **Redirects bounded** (5 by default), and an HTTPS→HTTP redirect is refused
  even under the opt-in.
- **Deadlines.** A whole-request timeout bounds a server that accepts the
  connection and then never sends a byte.
- **Response ceiling.** An oversized `Content-Length` is refused before the body
  is read, and a streaming byte count enforces the same ceiling when the header
  lies or is absent.
- **Local resource cap.** `Limits::max_target_bytes` refuses a manifest that
  declares an implausible target size, before any download. The manifest is
  unauthenticated, so its size claim is a request rather than a fact.
- **One workspace per update**, replacing shared fixed filenames, so concurrent
  updates cannot consume each other's files.
- **Atomic writes.** Downloads and reconstructions build into a `.part` file
  promoted by rename only after they pass their checks, so a failure can neither
  leave a finished-looking artifact nor destroy the one already there.

Breaking: `plan_update` takes a `Limits`; `Context` gains a `limits` field;
`HttpFetch::builder()` replaces direct construction for non-default policy.
