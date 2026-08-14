# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **A first release published no updater document.** `delta-release` required a
  predecessor, so a release with nothing to patch from could not be expressed,
  and the workflow gated the whole generate step on there being a previous tag —
  skipping the manifest and the signature with it. Tauri's `check()` had nothing
  to read until a second release happened to create one. The predecessor is now
  optional and the manifest is unconditional. Blocker **B5**; see
  `docs/DECISIONS.md` #32.

- **Generated patches were described but never applied.** The direct-patch
  generator recorded a patch's digest and size and emitted metadata for it,
  having never checked that applying it reconstructs anything. The tar layer had
  round-tripped its output from the start. Every patch now proves itself before
  publication, and a failure is a refusal to release. Blocker **B7**; see #33.

### Added

- **`release-check`**, a pre-upload gate. Reads the manifest back as a stranger,
  hashes the artifact about to be uploaded, verifies the signature under the
  configured public key, and refuses unless the document describes *this*
  release: tag, app id, platform, URL scheme, digest, size, the authenticated
  `delta-v1` identity, and the tar layer's representation. Replaces eleven lines
  of inline `python3` that could not be run locally or tested.

- **`delta-release --signature-out`**, writing the `.sig` beside the artifact as
  the rest of the Tauri ecosystem expects. Copied from the manifest rather than
  re-signed, since minisign is randomised.

- **Release rehearsals.** `examples/desktop-app/e2e/rehearse-release.sh` runs the
  workflow's command sequence over real `cargo tauri build` output for all three
  predecessor states; `github-hosted-e2e.sh` does the same against GitHub-hosted
  assets over HTTPS. `docs/RELEASING.md` documents the published asset set, the
  toolchain pins, and the Apple boundary.

### Changed

- **The release workflow publishes macOS `.app.tar.gz`.** It previously built a
  Linux AppImage that nothing had released or tested; macOS is the platform every
  claim here has been demonstrated on, and the only one where the tar layer
  applies. `tauri-cli` is pinned to an exact version rather than a caret range,
  and the job now rebuilds a retained published artifact before building anything
  — if the runner cannot reproduce a known artifact byte-for-byte, the release
  stops rather than shipping a tar layer no client could use.

### Security

- **Every stage of the tar pipeline now has a local ceiling.** Gate B bounded one
  quantity, the compressed installer, which predates the tar layer entirely. The
  reconstructed target tar's declared size reached zstd's window and output bound
  with nothing local above it, so a manifest could ask a client to reserve and
  write an arbitrary amount — spent long before any digest or signature check
  runs. `Limits::max_tar_bytes` closes it, as a dial independent of the cache's,
  because gzip's expansion ratio is unbounded and a ratio-derived cap inherits
  that. See `docs/DECISIONS.md` #29.

- **The blob store no longer infers a stored artifact from an occupied path.**
  `put` treated `AlreadyExists` as "the same bytes are already there", which holds
  for a file it wrote and not for a path anything running as this user can
  create. Planting a directory at a content address made staging report success.
  Nothing unsafe could be installed — reuse re-checks kind, size, digest and
  signature — but the bogus entry was promoted over a good base, permanently
  emptying the cache. See #30.

### Fixed

- **The full download raced itself.** `work_dir/full.artifact` was a fixed name
  under a shared directory, while both delta paths had had private workspaces
  since `docs/DECISIONS.md` #18 — which named this file and did not fix it. Since
  `HttpFetch` streams into a sibling `.part` and renames, concurrent updates
  could rename each other's in-flight downloads away, or read another release's
  bytes into their own signature check. Blocker **B4**; see #28.

- **Abandoned transaction workspaces are collected.** A crash mid-update stranded
  a full copy of the artifact under `work_dir` forever. Inert — the names are
  random, so nothing can find or misread one — but unbounded. Swept after 24
  hours, a threshold set far above any live transaction rather than copied from
  the cache's 60-second blob grace, because collecting a live workspace would
  break the update that owns it. See #31.

### Security

- **The release identity is now authenticated.** Minisign signature blocks carry
  a *trusted comment* covered by a second Ed25519 signature over
  `artifact_sig ‖ trusted_comment`, which `PublicKey::verify` — the call Tauri
  already makes — has always checked and nobody read. `delta-release` now writes
  a canonical identity there:

  ```text
  delta-v1 app:<id> v:<semver> plat:<os>-<arch> rep:<rep-id> b3:<64 hex> sz:<u64> ts:<unix>
  ```

  and the client checks every field against what it is actually installing. A
  signed artifact relabelled as a different version, platform, application or
  representation is now refused where before every cryptographic check passed.
  No second key, no second signature, no extra request. See
  `docs/DECISIONS.md` #27.

- **A contradicted identity fails closed and never falls back.** A release whose
  signed description disagrees with what it is being presented as is refused
  outright — it is not a transport problem, and treating it as one would let an
  attacker *choose* the full-download path.

- **Releases signed before this feature keep working, without the delta paths.**
  A signature carrying no `delta-v1` comment is a compatibility state, not an
  attack: the full download proceeds and only the delta paths are unavailable.
  This is safe because the comment lives inside signed bytes, so stripping an
  identity to force the legacy path invalidates the signature.

- **Not fixed, and stated plainly:** the manifest is still unsigned and freshness
  is still unproven. A genuine older release carrying its own genuine identity is
  refused for an existing installation by version policy, and nothing here
  establishes what "latest" means for a first install. This is not TUF-style
  freshness. See `research/FINDINGS.md` F28.

### Changed

- **BREAKING: the update flow is entered through `UpdateSession`.**
  `update.delta_session().install(&ctx, &fetch)` replaces constructing an
  identity and an installer separately. The session derives the identity from the
  checked `Update` and installs through *that same* `Update`, so the two cannot
  disagree. `UpdateIdentity` and `TauriInstall` are no longer part of the plugin's
  public API, which makes the previously expressible unsafe pairings fail to
  compile rather than fail review. Guarded by `compile_fail` doctests.
- **BREAKING: `delta-release` requires `--app-id`.** It is bound into the
  signature, so it cannot be inferred; the digest and size in the identity are
  derived from the artifact the tool just wrote, never from separately supplied
  values.

### Removed

- **BREAKING: `Builder::manifest_url`, and `Builder` configuration entirely.**
  Nothing had read that value since the flow started taking the release document
  from `Update::raw_json`. A required, security-adjacent knob that does nothing
  is worse than no knob. Use `Builder::new().build()`; point Tauri's updater at
  your manifest as you already do.

### Added

- **Tar-layer patching for macOS `.app.tar.gz`.** An optional `tar_layer` block
  on the existing platform entry describes patches against the *uncompressed*
  tar inside a compressed bundle. On three controlled builds the tar patch is
  ~15% of a full download where a direct patch is ~95% — 84% fewer patch bytes.
  Purely additive: existing `patches` are unchanged, and a client that does not
  implement the declared representation or recompression recipe reads them
  instead. See `docs/DECISIONS.md` #25.
- **Exact in-process rebuild of a published `.app.tar.gz`.** Replaying
  `tar::Builder`'s write topology into `flate2`'s `GzEncoder` reproduces the
  artifact `tauri-bundler` published byte-for-byte, so the existing minisign
  signature verifies against the rebuild. Pinned by a regression test against a
  real retained artifact. See `docs/DECISIONS.md` #26.
- **A persistent artifact cache**: content-addressed immutable blobs, plus
  ACTIVE/PENDING state behind a generation compare-and-set. `BlobStore::put`
  takes a `VerifiedArtifact`, so unverified bytes cannot be cached; `get`
  re-hashes and re-verifies the signature on every reuse against the currently
  configured key. Promotion happens on the next launch, from the version the
  running process reports about itself — never on `install()` returning `Ok`.
  See `docs/DECISIONS.md` #23 and #24.
- **`UpdateSource::TarDelta` and `Outcome::InstalledFromTarDelta`**, distinct
  from the direct-delta variants, and `UpdateSource::Full::attempted`, so a
  fallback test can assert the tar path was *tried* rather than never reached.
- `delta-release --tar-patch-out/--tar-patch-url/--require-tar-layer`. The
  generator applies its own patch, recompresses and requires byte-identity with
  the published artifact before emitting any metadata, and refuses to publish
  otherwise.

### Fixed

- **The cache state store treated an unreadable generation as an absent one.**
  `initialise` panicked on a state file written by a newer format — a plugin
  downgrade would have crashed the updater — and `load` fell back to the newest
  generation it could *parse*, resurrecting state the compare-and-set had
  already replaced and permanently wedging the store.
- A file was being ignored by a lowercase gitignore rule that differed from it
  only in case, because git matches ignore patterns case-insensitively where
  `core.ignorecase` is set. The rule is gone; the trap is worth remembering.

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

### Gate C — release tooling

- **`delta-release --version` renamed to `--target-version`.** The old name
  collided with clap's own flag, which made every invocation of a debug build
  panic — including `--help`. `tests/cli.rs` now runs the real executable.
- **The release workflow's tag path works.** It previously required four
  environment variables nothing set, so every tag push failed immediately. It now
  builds the AppImage, resolves and downloads the previous release, generates the
  patch and manifest, validates the manifest against the tag, and uploads.
- **Compatibility narrowed to what was verified.** `tauri-plugin-updater` is
  `>=2.10.1, <2.11.0`, enforced by a test that reads the resolved version from
  `Cargo.lock` and names the six upstream behaviours to re-read before widening.

Breaking: `--version` is now `--target-version`.
