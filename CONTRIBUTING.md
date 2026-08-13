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

You need Rust **1.88 or newer**. The floor moved from 1.85 when `tauri` entered
the workspace and its dependency tree raised it — see
[docs/DECISIONS.md](docs/DECISIONS.md) #12. CI reads `rust-version` from
`Cargo.toml`, so that field is the authority.

```sh
git clone git@github.com:Chahdane/tauri-updater.git
cd tauri-updater
cargo test --workspace --all-features --locked
```

**On Linux you need system libraries.** `tauri` pulls in `wry` and then
`webkit2gtk-sys`, which runs `pkg-config` at build time, so the workspace does
not compile without them:

```sh
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

macOS and Windows need nothing extra. (This section previously claimed the
project had no system dependencies at all. That stopped being true when the
plugin crate gained its `tauri` dependency.)

No network access is required. Test fixtures are generated deterministically
rather than downloaded, so everything runs offline and produces identical
results on every machine.

## Before opening a PR

These are the commands CI runs, verbatim. `--locked` matters: a lock file that
has drifted from `Cargo.toml` fails every job, which is how `main` was once red
on all five at once.

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

CI additionally re-runs two assertions **by name** and fails if they did not
execute — the cross-platform determinism test with its exact BLAKE3 digest, and
both `compile_fail` forgery doctests. A green suite is not by itself proof that a
load-bearing test still runs.

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

## Merging, and the one conflict that needs care

`main` is protected: every status check must be green before a merge. That is a
repo setting rather than a rule to remember, because the rule was forgotten once
and cost a day — see `docs/DECISIONS.md` #17.

**Never resolve a `docs/DECISIONS.md` conflict by taking one side wholesale.**
This includes GitHub's "Update branch" button, which does exactly that.

Decisions are numbered, and the numbers are referenced from code comments
(`docs/DECISIONS.md #13`) and from other decisions. Two branches that each add a
"#13" produce a conflict whose *natural* resolution — keep one side — silently
repoints every cross-reference at unrelated content. Nothing fails: it compiles,
the tests pass, and the docs now lie.

This happened. A web merge dropped two decisions while four code comments went on
citing them by number.

So when a merge touches `DECISIONS.md`:

1. Keep **both** sides. A decision that was worth recording does not stop being
   worth recording because someone else numbered one first.
2. Renumber the newer set, preferring to move whichever has **fewer inbound
   references** — check with `grep -rn 'DECISIONS[^ ]* #N' crates/ docs/ examples/ research/ README.md CONTRIBUTING.md`.
3. Walk **every** cross-reference against its new target before pushing:

   ```sh
   for n in $(grep -oE '^## [0-9]+' docs/DECISIONS.md | grep -oE '[0-9]+'); do
     printf '#%-3s %s\n' "$n" "$(grep -m1 "^## $n\." docs/DECISIONS.md)"
     grep -rn "DECISIONS[^ ]* #$n\b" crates/ docs/ examples/ research/ README.md CONTRIBUTING.md 2>/dev/null | sed 's/^/      /'
   done
   ```

   Read the output. A reference pointing at a plausible-but-wrong decision is the
   failure mode; it will not announce itself.

Documentation correctness is part of the security model here, not cosmetics — a
comment citing the wrong rationale is worse than no comment, because it is
trusted.

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
