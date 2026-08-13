# Controlled macOS `.app.tar.gz` fixture

`official-1.0.1.app.tar.gz` is a **real artifact**, not a constructed one. It was
produced by `cargo tauri build` (tauri-cli 2.10.1, tauri-bundler 2.8.1) from
`examples/desktop-app` at source commit `908e1d2`, during the run recorded as
`research/experiments/2026-08-13-macos-real-app-e2e-delta`, and retained
unmodified since.

| | |
| --- | --- |
| Size | 4,070,756 bytes |
| BLAKE3 | `718c104bfdcc00aaa7b588519ad62c2719a182d8e7364c1daf260ed8fbf96f36` |
| Contains | a 9,461,248-byte tar: 4 directories, 3 files, main binary 9,441,088 bytes |

`official-1.0.1.json` carries the minisign signature over those exact bytes and
the public key it was made with. The **private key is deliberately absent** — it
was generated per-run by the E2E harness and never committed, for the reason in
`docs/DECISIONS.md`: a signing key in a repository is the same hazard class as a
shipped test surface. Nothing here can sign; it can only verify.

## Why a 4 MB binary is in the tree

It is the only thing that can falsify the recompression recipe.

`crates/delta-core/src/recompress.rs` reproduces its expected output from
`tar::Builder` at test time, which proves the topology is *self-consistent* with
the writer this build links against. It cannot prove the topology matches what
Tauri's bundler actually published, because that artifact was produced by a
different binary, on a different day, from a different dependency graph.

`tests/macos_recompression.rs` closes that gap by rebuilding *this* file from the
tar inside it and requiring the result to be byte-identical, then requiring the
signature that was issued over the original to verify against the rebuild. A
regression in the recipe — a different chunk size, a padding rule, an encoder
backend swapped by feature unification — changes those bytes and fails the test.

No smaller fixture has that property.
