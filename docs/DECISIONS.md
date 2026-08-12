# Decisions

A log of choices that are not obvious from the code, why they were made, and what
would cause them to be revisited. Newest last.

---

## 1. Wrap `tauri-plugin-updater`; delta the installer artifact, not the install

**Decided:** 2026-08-11 · **Status:** active

Patch the artifact the official updater *downloads*, not the application that is
*installed*. In-place binary patching runs into locked executables on Windows,
invalidated code signatures and notarization on macOS, and needs to be crash-safe
against overwriting a user's working app.

Because the reconstructed file is bit-identical to the published one, Tauri's
signature verification and per-platform install logic run unchanged, and the
fallback is trivially "download it normally".

Full reasoning in [ARCHITECTURE.md](ARCHITECTURE.md).

---

## 2. No plugin crate until Phase 3

**Decided:** 2026-08-11 · **Status:** active

The workspace holds only `delta-core` until the plugin actually does something.

A scaffold crate whose `init()` returns an empty `Builder` is dead code — it has
no behaviour and no test can assert anything about it — and it would add several
minutes of `tauri` compilation to every CI run across three platforms, for zero
coverage. The seam it would occupy is described in ARCHITECTURE.md, which is
enough to review the design against.

**Revisit when:** Phase 3 starts and there is real install-handoff behaviour to
put in it.

---

## 3. BLAKE3, not SHA-256, for artifact verification

**Decided:** 2026-08-11 · **Status:** active

Verification runs over entire installer artifacts — hundreds of megabytes — on
the hot path of every update, and BLAKE3 hashes them several times faster than
SHA-256 while remaining a 256-bit cryptographic digest.

This is a *correctness* check, not the trust anchor. Authenticity comes from
minisign signatures, which are unaffected by this choice. Anything written into a
manifest must therefore name its algorithm explicitly rather than assume.

---

## 4. Minimum supported Rust version is 1.85

**Decided:** 2026-08-11 · **Status:** active

The floor was originally declared as 1.77, chosen by reading the `rust-version`
field of each direct dependency. That was wrong, and CI caught it: the `msrv` job
fails on `main`.

The real constraint is transitive and does not appear in any `rust-version`
field:

| Crate | Why it forces 1.85 |
| --- | --- |
| `constant_time_eq` 0.4.2 | `edition = "2024"`, and declares `rust-version = "1.85.0"` |
| `blake3` 1.8.6 | `edition = "2024"`; pulls in `constant_time_eq` |
| `cpufeatures` 0.3.0 | `edition = "2024"` (x86 targets only) |

Edition 2024 stabilized in Rust 1.85.0, so no earlier toolchain can even *parse*
these manifests. The failure happens during dependency resolution, before any of
this project's code is compiled:

```
error: failed to parse the `edition` key
Caused by:
  this version of Cargo is older than the `2024` edition, and only supports
  `2015`, `2018`, and `2021` editions.
```

Verified locally: `cargo +1.71.0 check` fails with the above; `cargo +1.85.0
check` and `cargo +1.85.0 test` both pass.

Everything *else* in the tree would allow 1.71 (`thiserror`, `proc-macro2`,
`quote`, `syn`, `unicode-ident`) or lower. Pinning `blake3` back to a 2021-edition
release would buy a lower floor at the cost of running an older hash
implementation, which is a bad trade for a crate on the verification path.

Note that `wasip2` declares `rust-version = "1.87.0"` — higher than our floor —
but it is only ever built for WASI targets, so it does not constrain native
builds.

**Revisit when:** `blake3` is replaced, or the ecosystem moves far enough that a
higher floor costs nothing.

**How this is enforced:** the `msrv` CI job reads `rust-version` straight out of
`Cargo.toml` rather than pinning a second copy, so the tested version and the
declared version cannot disagree. If a dependency later raises its own floor, the
job goes red rather than the requirement silently drifting up.

---

## 5. The manifest is a superset of Tauri's, not a second document

**Decided:** 2026-08-12 · **Status:** active

`manifest.json` carries Tauri's own `version`, `notes`, `pub_date` and
`platforms` fields untouched, with the delta information under a separate
`delta` key.

The alternative — publishing a delta manifest next to Tauri's — means two
documents that must agree about URLs, versions and signatures, updated by
different code paths. They *will* drift, and the failure mode is the worst one
available: a client that falls back to a full download and finds the fallback
description stale.

Making one document serve both readers removes the possibility. A delta-unaware
Tauri client reads this file and behaves exactly as it does today; the delta path
is additive. `the_published_manifest_is_a_valid_tauri_document` asserts that
property directly against the JSON the tool writes.

**Revisit when:** Tauri's static manifest format changes shape.

---

## 6. The signature covers the target installer, not the patch

**Decided:** 2026-08-12 · **Status:** active

The minisign signature in both layers is over the **installer that gets
installed** — never over the patch.

Both paths converge on the same artifact: a full download fetches it, the delta
path rebuilds it. Signing that artifact means one signature validates both, and
the delta path is incapable of installing anything the full-download path would
have rejected. Signing the *patch* instead would establish only that the patch
was authentic, which says nothing about what it produced.

**Constraints this imposes:**

- **Every published delta requires the release signing key.** There is no way to
  produce a valid manifest entry without it, and CI fails loudly rather than
  emitting an unsigned manifest.
- **Signing happens after the artifact is final.** The signature covers exact
  bytes, so anything that rewrites the installer afterwards — re-compression, a
  notarization staple — invalidates it and must run before this tool.
- **Re-signing is not idempotent.** Minisign includes randomness, so signing the
  same file twice yields different strings. Both layers are therefore written
  from a single signing operation per run, and `Manifest::validate` rejects a
  document where they disagree.
- **Not yet validated against a real Tauri release.** Signatures are produced by
  the `minisign` crate in prehashed mode and verified in tests by
  `minisign-verify`, the same crate Tauri's updater uses, with the same base64
  envelope. That is strong evidence but not proof of end-to-end compatibility.
  Phase 3 must confirm it against an actual Tauri updater.

---

## 7. Schema 1 patches only to the latest release

**Decided:** 2026-08-12 · **Status:** active

Every entry in a platform's `patches` map reconstructs the *current* release
directly. A user on any listed previous version applies exactly one patch.

Multi-hop chaining — 1.0.0 → 1.0.1 → 1.0.2 — would let a release publish one
small patch instead of one per supported previous version. It also means a client
applying several patches in sequence, where an intermediate result must be
verified before it can be used as the next base, and a single missing link
strands everyone downstream of it. Not worth it before there is evidence that
publishing N patches per release is a real cost.

Because of this, publishing a new version *replaces* the delta layer rather than
merging into it: entries that rebuild the previous release are stale the moment a
newer one ships. `a_new_release_replaces_patches_that_target_the_old_one` pins
that behaviour.

**Revisit when:** the number of supported upgrade paths makes per-release patch
generation expensive. Chaining would ship as `schema: 2`, and the `schema` field
exists so an older client refuses it cleanly instead of misreading it.

---

## 8. The window bound is enforced now, not deferred to Phase 4

**Decided:** 2026-08-12 · **Status:** active

Phase 1 left a gap: `apply` bounded the bytes it wrote, but not the window zstd
was allowed to allocate. A patch whose frame header declared a large window log
could force an allocation of up to 2 GiB before anything was validated.

The manifest now states the target installer's exact size, so the fix is
available immediately and is a few lines — derive the window log from
`max(base size, declared size)` instead of permitting zstd's maximum. Deferring
it to Phase 4 would have meant shipping a known allocation vector through an
entire phase for no benefit, so it is closed here.

The declared size doubles as a correctness gate: a patch that decompresses
cleanly to the wrong length now fails with `UnexpectedOutputSize` before the
digest is even computed.

**Note:** this bounds allocation for patches applied *through a manifest*. The
backend still accepts an unbounded call for use without one, which is what the
release tool and the Phase 1 round-trip tests use. Client code always goes
through `try_reconstruct`, which always supplies the size.

---

## 9. `try_reconstruct` cannot fail

**Decided:** 2026-08-12 · **Status:** active

The delta path's entry point returns a `Reconstruction`, not a `Result`. Its two
variants are "here is a verified artifact" and "download the full one".

The safety property — a bad patch can never break an install — is otherwise
enforced only by every caller remembering to catch every error. Making the
function infallible moves that from convention into the type system: there is no
error value that can escape the delta path and abort an update.
