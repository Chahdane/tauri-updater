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
**Validated against Tauri's actual verifier** (2026-08-12, the first task of
Phase 3, before any client flow was built on top of it).

`crates/delta-release/tests/tauri_signature_compat.rs` reproduces
`tauri-plugin-updater` 2.10.1's `verify_signature` and `base64_to_string`
**verbatim** — copied, not paraphrased — and runs them against signatures
`delta-release` actually produces. Paraphrasing would have hidden a real
difference: Tauri calls `PublicKey::decode` on the whole base64-decoded key
file, not `from_base64` on the key line, and a check written from memory would
plausibly get that wrong and still pass.

Findings:

- Tauri accepts the signature in **both** manifest layers.
- The signature is **prehashed** (`ED`, bytes `0x45 0x44`), asserted two
  independent ways: directly on the wire format, and by verifying with
  `allow_legacy = false`, which rejects any non-prehashed signature outright —
  so success there can only mean the prehashed branch ran. This matters because
  Tauri passes `allow_legacy = true`, meaning a merely-green verification would
  not have told us which code path executed.
- In `minisign-verify`, `verify` branches on `signature.is_prehashed` *before*
  `allow_legacy` is consulted, so prehashed signatures are accepted regardless.
- Tauri rejects a signature over different bytes, and one from another key.
- **An artifact rebuilt from a delta satisfies the same signature a full
  download would have been checked against** — which is the entire point of
  signing the target installer.

Version alignment: Tauri pins `minisign-verify = "0.2"` and so does this
workspace, so a shared build unifies on one implementation.

**Still not proven:** that a *running* Tauri application accepts an update
served this way. The gate above covers the algorithm; the example app is the
final confirmation and remains Phase 3 work. These are deliberately kept as
separate claims.

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

---

## 10. The plugin must verify the signature itself — Tauri will not

**Decided:** 2026-08-12 · **Status:** active · **Corrects an earlier assumption**

The original design said the reconstructed artifact "still has to satisfy Tauri's
minisign verification". Reading `tauri-plugin-updater` 2.10.1 while building the
handoff showed that is false, and it matters enough to record rather than quietly
fix.

`verify_signature` is called from **exactly one place** in the whole crate:

```rust
// src/updater.rs:712, inside Update::download()
verify_signature(&buffer, &self.signature, &self.config.pubkey)?;
```

The handoff seam goes nowhere near it:

```rust
// src/updater.rs:718
pub fn install(&self, bytes: impl AsRef<[u8]>) -> Result<()> {
    self.install_inner(bytes.as_ref())   // no verification
}
```

`download_and_install` is just `download()` then `install()`, so verification
belongs to the download step. The delta path exists precisely to avoid that
download — so **nothing verifies the signature unless this plugin does**.

The design survives; the reasoning had a hole. The plugin performs the check
itself before every handoff, using Tauri's verification reproduced verbatim and
pinned by `crates/delta-release/tests/tauri_signature_compat.rs`.

This promotes that compatibility gate from reassuring to load-bearing. It is no
longer "evidence we interoperate" — it is the only thing standing between a
reconstructed artifact and the installer. Any change to it needs the same rigour
as a change to the verification itself.

**Had this gone unnoticed**, the delta path would have become a way to install
artifacts with no signature check at all — the exact outcome the wrap-don't-
replace design exists to prevent, arrived at while believing the opposite.

**Revisit when:** upstream moves verification into `install`, or exposes a
verifying handoff. Then this becomes redundant and should be deleted rather than
left to rot alongside a second implementation.

---

## 11. Everything falls back except a signature failure

**Decided:** 2026-08-12 · **Status:** active

The delta path falls back to a full download on every failure — corrupt patch,
truncated patch, transport error, wrong base version, unknown backend, digest
mismatch, out of disk. **One case does not: a signature failure aborts.**

The precise rule is not "delta failures fall back". It is:

> Fall back on any failure that does not call the *manifest's authenticity* into
> question. A signature failure is the sole case where the manifest itself is
> under suspicion, so no fallback path exists.

Everything else is a statement about one artifact or one transfer, and says
nothing about whether the release document is genuine. A signature failure says
the opposite.

### Why there is nothing to fall back to

This follows directly from decision #5, the manifest-as-superset. The
full-download URL and the delta information live in **one document**, covered by
**one signature over the target installer**. So if that signature does not check
out, the document is untrustworthy in its entirety — including the
`platforms.<target>.url` a fallback would fetch from.

Falling back would download an artifact chosen by a document we have just decided
we cannot trust, and would then verify it against a signature from that same
document. It preserves the risk while presenting itself as the safe option, which
is worse than failing loudly: the user believes a safety mechanism engaged when
it did not.

### The load-bearing condition

**This reasoning depends on the full-artifact URL and hash sharing one signed
document with the delta information.** That is true of schema 1 and is a
consequence of #5.

If a future change ever splits them — a Tauri-native manifest plus a separate
delta sidecar, signed independently — **this asymmetry must be re-examined**. In
that world a delta-side signature failure would say nothing about the
Tauri-native document, and falling back could be both safe and correct. The
conclusion would change even though the code would still compile and every test
would still pass.

Not a change being made. Recorded so a later refactor cannot quietly reopen the
question by moving the ground the argument stands on.

---

## 12. MSRV moves to 1.88 when `tauri` enters the workspace

**Decided:** 2026-08-12 · **Status:** active · **Supersedes the floor in #4**

Adding `tauri` raised the minimum from 1.85 to **1.88**. CI caught it: the `msrv`
job went red on the branch that introduced the dependency, while all three test
jobs stayed green.

| Crate | Requires |
| --- | --- |
| `darling` 0.23.0 (and `_core`, `_macro`) | rustc 1.88.0 |
| `icu_collections`, `icu_locale_core` 2.2.0 | rustc 1.86 |

Verified rather than assumed: `cargo +1.85 check` fails naming those packages;
`cargo +1.88.0 check --workspace --all-targets --all-features --locked` passes.

Worth recording as a *design* note rather than a version bump: this is the price
of the wrap-don't-replace decision. Depending on `tauri` means inheriting its
whole dependency tree's floor, and that floor will keep moving. The engine
crates would still build on 1.85 — only the plugin needs 1.88 — so if a low MSRV
ever matters more than a single workspace-wide number, the split already exists
along the right seam.

**How this stays honest:** the `msrv` job reads `rust-version` from `Cargo.toml`,
so it tests whatever is declared. It caught this drift on the branch rather than
after release, which is the whole reason the job exists.
