# Sprint 3 — The Tauri plugin

**Phase:** 3 of 8 ([roadmap](docs/ROADMAP.md))
**Started:** 2026-08-12
**Status:** In progress

## Goal

First end-to-end delta update in a real Tauri app, on Linux/AppImage: the plugin
wraps `tauri-plugin-updater`, fetches the manifest, picks a patch, applies it,
verifies it, and hands the finished artifact to Tauri's own install step.

## Definition of done

- [ ] Green matrix — fmt + clippy + tests on Linux, macOS, Windows
- [ ] An example app updates itself from a patch
- [ ] Every failure path demonstrably falls back to a full download
- [ ] SPRINT / CHANGELOG / docs updated in the same PR

## Tasks

### Signature compatibility — done, and done first

This gated everything else: if Tauri rejected our signatures, the manifest format
and the whole handoff design would have needed rework, and finding that out after
building a client on top would have been expensive.

- [x] Reproduce `tauri-plugin-updater` 2.10.1's `verify_signature` verbatim and
      run it against real `delta-release` output
- [x] Confirm the signature is **prehashed**, proven two independent ways rather
      than inferred from a green test
- [x] Confirm Tauri rejects wrong bytes and wrong keys
- [x] Confirm a **delta-rebuilt** artifact satisfies the same signature
- [x] Record the result and the remaining gap in `docs/DECISIONS.md` #6

Result: cleared at the algorithm level. Six tests. The remaining gap — a
*running* Tauri app accepting an update served this way — is the example app
below, and is deliberately tracked as a separate claim.

### Plugin crate

- [ ] `tauri-plugin-updater-delta` crate wrapping `tauri-plugin-updater`
- [ ] Manifest fetch from a configured URL
- [ ] Patch selection for the installed version and current platform
- [ ] Patch download, with the manifest's digest checked before applying
- [ ] Reconstruct via `try_reconstruct`, then hand off
- [ ] Full-download fallback wired into every failure path

### Base artifact

- [ ] Cache the installer after a successful update, so the next update has a base
- [ ] Behave correctly when no base is available — plain full download, no error

### Install handoff

- [ ] AppImage adapter: hand the verified artifact to Tauri's install step
- [ ] Keep the seam narrow enough that Windows and macOS adapters slot in later

### Example app

- [ ] Minimal Tauri app using both plugins
- [ ] Serve a manifest and artifacts locally
- [ ] **Prove a real Tauri updater accepts a delta-rebuilt artifact end to end** —
      closes the remaining gap in DECISIONS #6
- [ ] Real-world patch ratio numbers for the README

### Docs

- [ ] README install and configuration, replacing the aspirational example
- [ ] ARCHITECTURE: the client flow as built
- [ ] DECISIONS: base-artifact caching strategy

## In progress

Plugin crate scaffold — now that it has real behaviour to hold, per DECISIONS #2.

## Blocked

Nothing.

## Notes

- `gh` is still not installed, so PRs are opened by hand. Reading CI *status*
  works anonymously now the repo is public; reading CI *logs* needs
  `gh auth login`.
- Two PRs are open and awaiting review: `fix/msrv`, and `feat/release-tooling`
  stacked on it. This sprint's branch stacks on those in turn.
- Phase 2 exit state: 63 tests green across Linux, macOS and Windows.

## Next sprint

Phase 4 — robustness and security: forcing every failure path in tests, resume
and retry for interrupted downloads, disk-space checks, and fuzzing the apply
path against malformed patches.
