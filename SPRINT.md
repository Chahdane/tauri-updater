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

- [x] `cargo test` green, including the AppImage round-trip
- [x] `cargo fmt --check` and `cargo clippy -D warnings` clean
- [ ] CI runs all three on every push and PR
- [x] Real patch-size numbers in the README
- [ ] Docs reflect what actually exists

## Tasks

### Scaffolding — done

- [x] Repo layout, Cargo workspace, `.gitignore`
- [x] MIT license, CONTRIBUTING
- [x] README, ARCHITECTURE, ROADMAP, CHANGELOG, this file
- [x] `delta-core` crate with error type
- [x] Content hashing + verification (`FileHash`, `verify_file`)

### Patch engine — done

- [x] `PatchBackend` trait
- [x] zstd backend via prefix-referencing (`--patch-from` equivalent)
- [x] Backend lookup by manifest id, with a clear error for unknown ids
- [x] Bounded decompression output — a hostile patch cannot force an unbounded write
- [x] Streaming apply, so the reconstructed artifact is never held in memory

### Tests — done

- [x] Deterministic AppImage-shaped fixture generator (offline, reproducible)
- [x] **Flagship:** round-trip — diff → apply → `hash(rebuilt) == hash(new)`
- [x] Patch is materially smaller than the full artifact
- [x] Applying against the *wrong* base version fails verification
- [x] Applying a corrupted patch errors instead of emitting a wrong file
- [x] Truncated patch reported as truncation, not silent success
- [x] Output ceiling enforced
- [x] Edge cases: empty file, single-byte file, identical versions

21 tests, all green.

### CI — done

- [x] GitHub Actions: fmt + clippy + test on push and PR
- [x] Cargo registry/build caching
- [x] Tests run on Linux, macOS and Windows — the only evidence the engine really
      is platform-agnostic, given development happens on macOS only
- [x] MSRV job that builds at the declared `rust-version` (1.77; highest
      dependency floor is thiserror at 1.71)
- [x] Dependabot for cargo and actions

### Docs

- [x] README benchmark section filled in with measured numbers
- [ ] ARCHITECTURE: record the residual window-allocation limit found during
      implementation (deferred to Phase 4)

## In progress

Nothing — Sprint 1 work is complete and awaiting review. Three PRs are open and
stacked: `chore/scaffold` → `feat/patch-engine` → `chore/ci`.

## Blocked

Nothing.

## Notes

- `gh` CLI is not installed on the dev machine yet, so PRs are opened by hand
  until it is. Push access over SSH works.
- Patch-size numbers must come from a reproducible test in this repo, not quoted
  from other projects.
- Measured: 6,815,744 byte artifact → 393,782 byte patch (5.78%). Of that,
  393,216 bytes are genuinely new data, so the engine's overhead over the
  theoretical minimum is 566 bytes. The interesting result is the overhead, not
  the ratio — the ratio is a property of the fixture.
- Found during implementation: `apply` bounds its *output*, but a patch declaring
  a large window log can still make zstd allocate a window of up to 2 GiB. Real
  fix is to bound it by the artifact size the manifest declares, which does not
  exist until Phase 2. Tracked for Phase 4.

## Next sprint

Phase 2 — patch manifest format and the release-side CLI that generates patches
from a set of published artifacts.
