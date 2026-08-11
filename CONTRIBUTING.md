# Contributing

Thanks for taking a look. This is an early-stage project, so the most valuable
contributions right now are testing on platforms the maintainer cannot reach and
arguing with the design before it hardens.

## What would help most

- **Windows and Linux testing.** Development happens on macOS with no Windows
  machine available. Anything that exercises the real install path on those
  platforms is worth more than a feature.
- **Real-world artifacts.** Patch ratios depend enormously on how an app is
  built. Measurements from actual Tauri apps sharpen the benchmarks.
- **Design review.** See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). If the
  wrap-don't-replace approach has a hole in it, that is better found now.

## Getting set up

You need a recent stable Rust toolchain (1.77 or newer):

```sh
git clone git@github.com:Chahdane/tauri-updater.git
cd tauri-updater
cargo test
```

There are no system dependencies and no network access required for the test
suite. Test fixtures are generated deterministically rather than downloaded, so
everything runs offline and produces identical results on every machine.

## Before opening a PR

All three of these run in CI and must pass:

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Branching and commits

The project follows GitHub Flow: `main` is always releasable and is never
committed to directly.

- Branch per unit of work, named by type: `feat/…`, `fix/…`, `docs/…`,
  `chore/…`, `test/…`, `refactor/…`.
- Commits follow [Conventional Commits](https://www.conventionalcommits.org/):
  `feat: add zstd patch backend`, `fix: bound decompression output`.
- Keep commits small and logically scoped. A reviewer should be able to read one
  commit and understand one change.
- Open a PR into `main` and explain *why* in the description. Non-obvious
  decisions belong there, not in code comments.

## Code standards

- Idiomatic Rust with clear module boundaries. No dead code, no speculative
  abstractions.
- Every core piece has tests. The round-trip test is the flagship and must never
  be weakened to make something else pass.
- Errors on the delta path are always recoverable: they fall back to a full
  download. Nothing there may panic on malformed or hostile input.
- No telemetry, ever. Offline-first is part of the pitch.
- Explain non-obvious decisions in the PR description rather than in inline
  comments.

## Tests

The round-trip test is the heart of the suite: generate two versions of an
artifact, diff them, apply the patch, and assert the rebuilt file hashes
identically to the original. Any change to the patch engine needs it to stay
green, plus a test for whatever new failure mode the change introduces.

Failure cases matter as much as the happy path. A patch engine that silently
produces a *slightly wrong* file is far more dangerous than one that errors out,
so tests for corrupt patches and mismatched base versions are not optional.

## License

Contributions are licensed under the MIT license, the same as the project.
