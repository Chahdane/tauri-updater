# Controlled macOS `.app.tar.gz` fixture

`official-1.0.1.app.tar.gz` is a **real artifact**, not a constructed one. It was
produced by `cargo tauri build` of `examples/desktop-app` 1.0.1 on a GitHub
`macos-latest` runner, with tauri-cli 2.10.1 installed by
`cargo install tauri-cli --version =2.10.1 --locked` — the way every workflow
and `docs/RELEASING.md` install it. That lockfile builds the bundler with
flate2 1.1.1, zlib-rs 0.5.0 and tar 0.4.43, the versions this workspace pins.
Regenerate it with `examples/desktop-app/e2e/make-macos-fixture.sh` (the
`fixture` input of the manual benchmark workflow).

| | |
| --- | --- |
| Size | 4,047,020 bytes |
| BLAKE3 | `8cb0401699c6872e89332834f5fb8ce246b8df434bf38882af95ca776c35344a` |
| Contains | a 10,234,368-byte tar |
| Built by | tauri-cli 2.10.1 (`--locked`), rustc 1.98.1, benchmark run 36175835119 |

It replaces an earlier fixture built by a tauri-cli installed **without**
`--locked`, whose bundler resolved flate2 1.1.9 / zlib-rs 0.6.7 and wrote
different gzip bytes. See `docs/DECISIONS.md` #42.

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
