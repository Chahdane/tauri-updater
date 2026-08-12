# Sprint 2 — Release tooling and the manifest

**Phase:** 2 of 8 ([roadmap](docs/ROADMAP.md))
**Started:** 2026-08-12
**Status:** Complete — awaiting review

## Goal

Make patches producible by a real release pipeline, and close the loop: a
manifest generated from two installers must be enough for a client to rebuild
the new installer from a patch, verify it against both a hash and a signature,
and fall back to a full download whenever anything is wrong.

Still no Tauri dependency. Everything here is file-in/file-out and runs on macOS.

## Definition of done

- [x] Green matrix — fmt + clippy + tests on Linux, macOS, Windows
- [x] End-to-end manifest → apply → verify loop passes, with fallback asserted
      for corrupt, truncated and wrong-base patches
- [x] `cargo fmt --check` and `cargo clippy -D warnings` clean
- [x] SPRINT / CHANGELOG / docs updated in the same PR
- [x] Phase 1's window-log gap closed using the manifest's declared size

## Tasks

### Shared fixtures

- [x] Move the AppImage fixture generator into its own unpublished crate so both
      `delta-core` and the release tool test against the identical artifacts

### Manifest

- [x] Superset of Tauri's static updater JSON — `version`, `pub_date`,
      `platforms -> { url, signature }` — so the full-download fallback *is* the
      official updater's manifest and the two cannot diverge
- [x] Delta layer: `{ platform, from_version } -> { target_version, patch_url,
      patch hash, target installer hash + size, backend_id, signature }`
- [x] Hash fields named by algorithm, plus an explicit `hash_algo` — no silent
      BLAKE3-vs-SHA256 mismatch
- [x] Reject a manifest whose delta signature disagrees with its Tauri-layer
      signature, since both describe the same artifact
- [x] v1 is patch-from-any-previous-to-latest only; multi-hop chains are future
      work

### Signing

- [x] Minisign signature over the **target installer**, using the scheme Tauri's
      updater verifies, so the delta handoff and the fallback validate against
      the same signature
- [x] Verify in tests with `minisign-verify` — the same crate Tauri uses — rather
      than round-tripping through our own signer

### Release tool

- [x] `delta-release` binary: previous installer + new installer → patch, hashes,
      signature, and a written/updated `manifest.json`
- [x] Updating an existing manifest adds a patch path without discarding others
- [x] Tests over the tool's logic, not just its plumbing

### Engine

- [x] Bound zstd's window and output allocation using the target installer size
      the manifest now carries — closes the Phase 1 gap
- [x] `try_reconstruct` that cannot return an error: the delta path either
      produces a verified artifact or reports that a full download is required

### Publish workflow

- [x] `release.yml` — runs the tool on a tagged release, uploads patch + full
      installer + manifest
- [x] Dry-runnable, since no real tagged release can be cut yet

### Docs

- [x] DECISIONS: signature-over-target-installer and the constraint it imposes
- [x] DECISIONS: manifest redundancy between the Tauri and delta layers
- [x] ARCHITECTURE: manifest format and the release-time flow
- [x] ROADMAP / CHANGELOG / README

## In progress

Nothing — sprint 2 work is complete and awaiting review on `feat/release-tooling`,
stacked on `fix/msrv`.

## Blocked

Nothing.

## Outcome

57 tests, all green, on Linux, macOS and Windows.

Three traps in the `minisign` crate, each found by a test rather than in
production:

- `password: None` means *prompt on the terminal*, so a release job would die
  with `Device not configured` or hang forever waiting for input. `None` is now
  translated to an empty password.
- `SecretKeyBox::from_string` accepts any string and only fails later, so the
  "try raw, fall back to base64" approach for Tauri-encoded keys never reached
  the fallback. Key text is now decided by content.
- `KeyPair::generate_unencrypted_keypair()` leaves the secret key checksum
  zeroed, and `write_checksum` is private, so its output cannot be loaded again.
  Tests generate with an empty password instead.

The Phase 1 window-log gap is closed rather than deferred — the manifest carries
the target size, so the fix was a few lines and shipping a known allocation
vector through another phase bought nothing.

## Notes

- `gh` is still not installed on the dev machine, so PRs are opened by hand.
  Reading CI *status* works anonymously now the repo is public; reading CI *logs*
  needs `gh auth login`, which is why cross-platform determinism is asserted in
  the test suite rather than compared across logs by eye.
- Phase 1 exit numbers, for reference: 6,815,744 byte artifact → 393,782 byte
  patch, reproduced byte-identically on Linux, macOS and Windows.

## Next sprint

Phase 3 — the Tauri plugin itself: manifest fetch, patch download, base-artifact
caching, and the AppImage install handoff.
