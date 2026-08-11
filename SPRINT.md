# Sprint 1 — The engine

**Phase:** 1 of 8 ([roadmap](docs/ROADMAP.md))
**Started:** 2026-08-11
**Status:** In progress

## Goal

Prove on this machine, offline, that a patch can rebuild an update artifact
byte-for-byte — and that wrong or corrupt inputs fail loudly instead of producing
a plausible-but-wrong file.

Nothing in this sprint touches Tauri. The engine is platform-agnostic, so all of
it is verifiable on macOS despite Linux being the first target platform.

## Definition of done

- [ ] `cargo test` green, including the AppImage round-trip
- [ ] `cargo fmt --check` and `cargo clippy -D warnings` clean
- [ ] CI runs all three on every push and PR
- [ ] Real patch-size numbers in the README
- [ ] Docs reflect what actually exists

## Tasks

### Scaffolding — done

- [x] Repo layout, Cargo workspace, `.gitignore`
- [x] MIT license, CONTRIBUTING
- [x] README, ARCHITECTURE, ROADMAP, CHANGELOG, this file
- [x] `delta-core` crate with error type
- [x] Content hashing + verification (`FileHash`, `verify_file`)

### Patch engine — in progress

- [ ] `PatchBackend` trait
- [ ] zstd backend via prefix-referencing (`--patch-from` equivalent)
- [ ] Backend lookup by manifest id, with a clear error for unknown ids
- [ ] Bounded decompression output — a hostile patch cannot force a huge allocation

### Tests

- [ ] Deterministic AppImage-shaped fixture generator (offline, reproducible)
- [ ] **Flagship:** round-trip — diff → apply → `hash(rebuilt) == hash(new)`
- [ ] Patch is materially smaller than the full artifact
- [ ] Applying against the *wrong* base version fails verification
- [ ] Applying a corrupted patch errors instead of emitting a wrong file
- [ ] Edge cases: empty file, single-byte file, identical versions

### CI

- [ ] GitHub Actions: fmt + clippy + test on push and PR
- [ ] Cargo registry/build caching

### Docs

- [ ] README benchmark section filled in with measured numbers
- [ ] ARCHITECTURE updated if the trait shape changes during implementation

## In progress

Patch engine — `PatchBackend` trait and the zstd backend.

## Blocked

Nothing.

## Notes

- `gh` CLI is not installed on the dev machine yet, so PRs are opened by hand
  until it is. Push access over SSH works.
- Patch-size numbers must come from a reproducible test in this repo, not quoted
  from other projects.

## Next sprint

Phase 2 — patch manifest format and the release-side CLI that generates patches
from a set of published artifacts.
