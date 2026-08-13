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

**Decided:** 2026-08-12 · **Status:** active · **Reasoning corrected 2026-08-12**

The delta path falls back to a full download on every failure — corrupt patch,
truncated patch, transport error, wrong base version, unknown backend, digest
mismatch, out of disk. **One case does not: a signature failure aborts.**

The policy stands. The original argument for it did not, and the correction
matters more than the conclusion, so both are recorded.

### What the original reasoning got wrong

This decision used to say:

> if that signature does not check out, the document is untrustworthy in its
> entirety

That claim cannot be supported, because it describes a mechanism that does not
exist. **The manifest is not signed.** There is no signature over the document,
so a failed *artifact* signature cannot invalidate it — there was never a claim
about the document to disprove. The document was unauthenticated before the
failure and is exactly as unauthenticated after it.

The error was conflating two properties that a single minisign check does not
combine:

| | What it means | What our signature actually gives us |
| --- | --- | --- |
| **Artifact authenticity** | These *bytes* were signed by the expected key | **Yes** |
| **Manifest authenticity** | This *description* — version, URL, sizes, digests — came from the release process | **No** |

A signature over the installer covers the installer. It covers no manifest
field, so it authenticates no manifest field.

### The corrected argument for the same policy

A signature failure means the relationship between the artifact bytes, the
supplied signature and the trusted public key does not hold. That is evidence
something in the chain that produced this release is wrong. It does not tell us
*what* is wrong, and — this is the part that decides the policy — it gives us no
way to localise the fault.

Falling back would fetch a second artifact chosen by the same unauthenticated
document and check it against a signature from that same document. If the
document is being manipulated, that is a second attempt at the same attack down
a different branch; if the release tooling is broken, it is a second draw from
the same broken process. Neither is safer than the first attempt, and both
present themselves as a safety mechanism having engaged.

Note what this argument does **not** rely on: it never claims the document has
been proven false. It only claims that a failure of unknown origin does not
license a retry through the same unverified channel. That holds whether or not
the manifest is authenticated, which is why the policy survives the correction.

### The load-bearing condition

**This reasoning depends on the full-artifact URL and the delta information
arriving in one document, from one fetch.** That is true of schema 1 as a
consequence of #5, and is now enforced structurally by #13 rather than left to
convention.

If a future change ever splits them — a Tauri-native manifest plus a separately
signed delta sidecar — **this asymmetry must be re-examined**. In that world a
delta-side failure would say nothing about the Tauri-native document, and
falling back could be both safe and correct.

Recorded so a later refactor cannot quietly reopen the question by moving the
ground the argument stands on.

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

---
## 13. One release, described once

**Decided:** 2026-08-12 · **Status:** active · **Supersedes the flow in #5**

Found by an independent adversarial audit, then reproduced against this
repository and against `tauri-plugin-updater` 2.10.1.

### The defect

The example fetched the release manifest **twice**: once inside
`Updater::check()`, and again inside `run_update`, which took a manifest URL.
Nothing compared the two responses. They were two independently obtained
descriptions of what the update *is*, and they controlled different things:

| Question | Answered by |
| --- | --- |
| Is there an update at all? | Tauri's fetch, via `release.version > current_version` |
| Which version do we patch toward? | **Our fetch** |
| Which URL is the full fallback? | **Our fetch** |
| Which signature is checked? | **Our fetch** |
| Which bytes get installed? | Whatever we hand `Update::install` |

So an attacker who could rewrite the manifest response — no signing key
required — could answer Tauri's request with `version: 9.9.9` so the semver gate
passed, and ours with a genuinely signed *older* release. Verification succeeds,
because an old release's signature is valid and always will be. The user is
silently downgraded to a known-vulnerable version.

Signature verification cannot catch this. Minisign signatures carry no version
and never expire, so "these bytes were signed by us" is true of every artifact
we have ever shipped.

### The fix, and why it needed no new protocol

`Update` already retains the whole document it fetched, verbatim, on
`raw_json` (`updater.rs:492`, `:552`), along with the version, target, URL and
signature it selected from it. Since our manifest is a superset of Tauri's (#5),
**our delta layer is already inside the response Tauri fetched.**

So the second fetch was not just dangerous, it was redundant. `UpdateIdentity`
carries those fields forward, `run_update` takes one and has no URL parameter at
all, and the delta layer is parsed out of `raw_json`. One document, one release:

```text
checked_target == delta_target == verified_target == installed_target
```

The first three equalities are structural — they are views of one parse, and
`Manifest::validate` already enforces internal consistency. The fourth is closed
by `VerifiedArtifact` owning the bytes it authenticated (#10).

### The two seams that remain, and are checked

One document can still be read two ways:

- **Our parse vs Tauri's.** `manifest.version` must equal
  `identity.version()`.
- **Our platform entry vs Tauri's selection.** Tauri searches
  `{os}-{arch}-{installer}` before `{os}-{arch}` (`updater.rs:1420`) and does not
  report which key won. Reproducing that search here would create a second
  selection algorithm free to drift from the real one, so instead the delta entry
  we look up must carry the signature Tauri selected.

Both refuse rather than fall back, for the reason in #11.

### Rejected alternatives

- **Sign the manifest with a second key.** New key management, and it breaks the
  superset property that makes delta-unaware clients work. Solves nothing that
  using the already-fetched document does not.
- **Fetch twice and compare.** Still two round trips, still a window between
  them, and a server can simply serve a consistent malicious pair. Does not
  address downgrade at all.
- **Bind the delta metadata into the signed artifact.** Impossible: the metadata
  must be readable *before* the artifact is obtained.
- **Keep our fetch authoritative and treat `Update` as advisory.** Discards
  Tauri's semver gate and its platform selection — the wrong direction of trust.

### The cost

The delta path is now reachable only *through* Tauri's check. There is no
standalone "check for delta updates" entry point, and for v0.1 that is
deliberate: such an API would independently decide which release to install, and
would need its own threat model and version policy before it could be safe.

**Revisit when:** upstream stops retaining `raw_json`, or exposes which platform
key `get_urls` selected — the latter would let the signature cross-check become
a direct key comparison.

---

## 14. Version policy is ours, not Tauri's to lend

**Decided:** 2026-08-12 · **Status:** active

`plan_update` looked patches up by raw string (`patches.get(from_version)`) and
never compared the installed version to the target. The only version policy
anywhere was upstream's `release.version > self.current_version` — inside a call
this crate did not make.

That was survivable only while every caller went through Tauri's check first,
and #13 shows that even then it constrained the wrong document. So the policy
lives here now, explicit and applying to every caller:

| Case | Outcome |
| --- | --- |
| `current < target` | Proceed |
| `current == target` | `UpToDate` — not an error |
| `current > target` | **Refuse** (`Refusal::Downgrade`) |
| Either version unparseable | **Refuse** (`Refusal::UncomparableVersion`) |
| Prereleases | Semver ordering — `1.0.0-beta < 1.0.0` |
| Several versions behind | Proceed; schema 1 patches straight to the latest |

### Refusal is not fallback

`UpdateSource::Refused` is deliberately a distinct variant rather than a
fallback, and `plan_update` stays infallible. A delta failure says a transfer
went wrong; a refusal says the release we are being *pointed at* is wrong, and
the full-download path is pointed at a release by the same document. Letting a
refusal decay into a full download would run the downgrade attack down the other
branch.

An unparseable version refuses for the same reason rather than falling back:
without an ordering the policy cannot be applied at all, and proceeding without
it reopens exactly the hole it closes.

### One deliberate divergence from the specification

SemVer §10 says build metadata MUST be ignored when determining precedence. The
`semver` crate does not implement that in `Ord` — it orders by build metadata,
so `1.0.0+build.2 > 1.0.0+build.1`. Verified, not assumed.

We match the crate rather than the specification, because
`tauri-plugin-updater` gates updates with `>` on this same crate. Implementing
the spec here would let the two disagree about whether a release is newer, which
is a policy seam of precisely the kind #13 exists to remove. Pinned by a test
that names the surprise, so upstream changing its ordering reopens the decision
instead of silently drifting.

We also strip a leading `v` before parsing, because upstream's `parse_version`
does (`updater.rs:1443`); not doing so would refuse releases Tauri accepts.

---
## 15. Delta is correctness-only on macOS until the tar layer is addressed

**Decided:** 2026-08-12 · **Status:** active, tracked as Phase 4/5 work

Measured on the real example app, two versions built by `cargo tauri build` and
published by `delta-release`:

| Platform | Artifact | Patch | Full | Patch as % of download |
| --- | --- | ---: | ---: | ---: |
| Linux | `.AppImage` (synthetic fixture) | 393,782 | 6,815,744 | **5.78%** |
| macOS | `.app.tar.gz` (real bundle) | 3,860,122 | 4,057,308 | **95.14%** |

macOS saves almost nothing. This was predicted in ARCHITECTURE before it was
measured, and the measurement confirms the mechanism rather than revealing a bug.

### Why

`.app.tar.gz` is **gzip-compressed**, and gzip (DEFLATE) is a streaming format:
its output at any point depends on everything that came before. Change a few
bytes in the main binary and every compressed byte after that point shifts —
different back-references, different Huffman coding, different bit alignment. The
uncompressed tars are nearly identical; the compressed ones share almost nothing.

The engine reuses whatever the two inputs genuinely share. Here they share very
little *as presented*, so a 95% patch is the correct answer to the question being
asked. The question is wrong, not the answer.

### The fix, already sketched

Delta the **uncompressed tar**, then reproduce the gzip layer deterministically
so the reconstructed `.app.tar.gz` is byte-identical to the published one — which
it must be, because the signature covers those exact bytes (#6). That is more
involved than it sounds: it requires pinning the compressor's parameters so the
same input always yields the same output, and confirming Tauri's bundler is
reproducible in the same way.

Phase 4/5 work. **Not a Phase 3 blocker** — a 95% patch exercises the install
path exactly as a 5% one does, so it does not weaken the end-to-end proof at all.

### What this means for adopters

State ratios per platform, never as one number:

- **Linux / AppImage** — the intended case, and the engine performs as designed.
- **macOS** — correct but not yet worth enabling; wait for the uncompressed-tar
  work.
- **Windows** — unmeasured. NSIS `.exe` and `.msi` are also compressed
  containers, so expect something closer to macOS than Linux until measured.

A single "delta updates for Tauri" claim that quietly averages these would
overpromise on two platforms out of three.

---

## 16. rsign2 and the `minisign` crate disagree about empty passwords

**Decided:** 2026-08-12 · **Status:** active

A key produced by `tauri signer generate --password ""` **cannot be loaded by
`delta-release` at all** — not with an empty password, not with any password. It
fails with `Wrong password for that key`, which is a baffling thing to read about
a key that has no password.

This is not a Tauri bug. Tauri signs with that key perfectly well.

### The divergence

Tauri's CLI signs with **rsign2**; this project uses the **`minisign` crate**.
Both write the same file format, and the key's header confirms the encryption
fields are populated (`kdf_alg: Sc`, non-zero salt). They differ in one place:

- **rsign2** runs its KDF over the password whatever it is, including `""`.
- **`minisign`** skips encryption entirely when the password is empty:
  `if !password.is_empty() { sk = sk.encrypt(password)? }`.

So rsign2 writes a key encrypted under the empty password; `minisign` then reads
it without decrypting and the checksum check fails. Same format, same input,
different behaviour.

### Why it is worth a decision entry

This is precisely the failure that kills adoption quietly. Someone follows
Tauri's own documentation, accepts the default of no password, gets an error that
appears to be about a password they never set, and concludes the tool is broken.

Handled in two ways: the error now names the cause and the remedy, and a test
pins the behaviour so a version bump in either library cannot change it
unnoticed.

**Revisit when:** either library changes its empty-password handling — at which
point the test fails and this entry explains why it was ever there. The
longer-term fix is to sign with rsign2 directly, guaranteeing key compatibility
with whatever Tauri's CLI produces.

---

## 17. `feat/e2e-real-app` was merged into `main` out of order

**Decided:** 2026-08-12 · **Status:** active · **Records a process mistake**

PR #12 (`feat/e2e-real-app`) was merged into `main` while PR #13 (Gate A,
`fix/update-identity`) was still open. It was meant to be a backup push only —
the branch was pushed so eight commits of unfinished Phase 4 work were not
living on one laptop, and the `pull/new/<branch>` link GitHub prints on push was
followed by mistake.

Two consequences, both recorded here rather than quietly absorbed:

- **`main` contains Phase 4 example-app code ahead of the roadmap.** The example
  app, the two-version build script and the E2E harness scripts are in `main`
  while `docs/ROADMAP.md` and `README.md` still describe them as unstarted. That
  is a documentation mismatch, not a code problem, and reconciling it is
  explicitly part of the Gate D truthfulness audit.
- **The example forced the Gate A API port early.** `examples/desktop-app` is a
  workspace member, so merging Gate A would not compile until the example moved
  to `UpdateExt::delta_identity`. That port was already planned as the first
  task of the E2E resume; it simply happened sooner, and is included in the Gate
  A PR.

Not reverted. Reverting a merge would trade working code for tidier history, and
the E2E work in it is real and wanted. Recorded so that a later reader does not
try to reconcile the roadmap against `main` and conclude the roadmap is lying.

**Revisit when:** Gate D reconciles the status tables. This entry can then be
marked historical.

---

---

## 18. A workspace per update, not a lock

**Decided:** 2026-08-13 · **Status:** active

Every update used the same three filenames — `update.patch`, `update.artifact`,
`full.artifact` — under a `work_dir` the only real caller set to a fixed shared
path. Nothing coordinated access to them.

### What actually breaks, and what does not

Not integrity. Traced end to end: run A writes `update.artifact` and passes the
digest gate; run B overwrites it; A reads the file and its **signature check
fails**. Since `VerifiedArtifact` owns the bytes it authenticated (#10), a
swapped file can only cause a *failure*, never a bad install.

But that failure is `Error::Signature`, which #11 makes the one non-recoverable
outcome. So a user double-clicking "Check for updates" could turn a benign race
into a loud, deliberately un-retried abort. **Availability, not integrity** — and
that distinction sets how much machinery is worth spending.

### Why not a lock

An advisory file lock was the other candidate. Rejected:

- Both crates are `#![forbid(unsafe_code)]`, so `flock` means a new dependency,
  plus stale-lock recovery after a crash, plus a deadlock surface — real
  complexity against a threat that is a double-click.
- It serialises access to a shared name. Giving each update its own name removes
  the sharing instead, and no contention beats coordinated contention.
- The exposure is genuinely small: a desktop app, one user, updates triggered by
  a click. Building for cross-process contention here would be building for a
  threat this shape of application does not have.

### What was built

`tempfile::Builder::tempdir_in(work_dir)` per `plan_update`, owned by
`UpdateSource::Delta` so it lives exactly as long as the artifact is needed. One
mechanism closes three findings: unique paths (concurrency), automatic removal
(partial-file cleanup), and nothing at a fixed valid-looking name (the crash half
of atomic handling). `tempfile` moves from dev- to runtime dependency; no new
third-party code enters the tree.

Reconstruction additionally builds into a `.part` file and renames it into place
only after the digest gate, so the artifact path never holds unverified bytes.

**Revisit when:** update checks become automatic *and* frequent, or the plugin
starts caching base artifacts in a shared location that two processes could write
at once. Either would make real contention likely rather than incidental, and a
lock would then be earning its cost. Until then it would be machinery for a
threat we do not have.

---

## 19. The transport policy is Tauri's, applied to every URL

**Decided:** 2026-08-13 · **Status:** active

`HttpFetch` permitted plain HTTP, followed redirects wherever they led, had no
read or overall deadline, and no size ceiling — it streamed whatever arrived
straight to the destination path.

### Mirroring upstream rather than inventing

`tauri-plugin-updater` already answers the HTTPS question (`config.rs:145`):
allowed outright when `dangerousInsecureTransportProtocol` is set, warned and
allowed in a development build, refused in a release build. We adopt that policy
and **that name**, deliberately.

This is the same reasoning as matching Tauri's version comparator in #14. Where
our policy and Tauri's answer the same question, they must not be able to
disagree — a developer who opted in for the updater and then meets a second,
differently-named refusal from us has been handed exactly the kind of seam Gate A
existed to close.

Two places we are deliberately stricter, both free:

- **Every URL, not just endpoints.** Upstream validates the manifest endpoints
  and then fetches `download_url` with no scheme check at all. We control the
  patch URL and the artifact URL, so both are checked.
- **No HTTPS→HTTP redirect, ever.** Not even under the opt-in. Starting on plain
  HTTP is a choice the app made; being moved off HTTPS mid-chain is a choice the
  server made, and those are not the same thing.

Rejected: exempting `localhost`/`127.0.0.1` automatically. An implicit rule
someone has to know is the antipattern; and anyone who can bind localhost has
already won. The explicit flag is the same escape hatch without the ambiguity.

### The harness was already broken, which is why this is not a weakening

`e2e/build-versions.sh` runs `cargo tauri build` — a **release** build — and the
example's `tauri.conf.json` served `http://127.0.0.1` endpoints without setting
`dangerousInsecureTransportProtocol`. Upstream's own `validate_endpoints` refuses
that combination in release, so **the preserved E2E harness could never have run
as configured**, independently of anything in this gate.

Found by reading `tauri-plugin-updater`'s source rather than assuming its
behaviour — the sixth time in this project that reading upstream replaced a
plausible belief with a certain one, after the `PublicKey::decode` encoding, the
`install()`-does-not-verify finding, the double-fetch trust seam, `raw_json`
being retained, and `tauri-build` requiring a Windows `.ico`. The pattern is
worth naming: when the evidence is one `grep` away in `~/.cargo/registry`,
guessing is a choice.

So the example now sets the flag, with a warning next to it. That makes an
implicit dependency explicit; it does not introduce new insecurity. The
alternative — leaving it out — means E2E stays blocked on folklore setup, which
is strictly worse than a scary flag with an explanation attached.

### Bounds, and where the numbers came from

| Bound | Default | Why |
| --- | --- | --- |
| `max_response_bytes` | 2 GiB | Above any plausible installer, far below "fills the disk" |
| `request_timeout` | 30 min | Generous: a big artifact on a slow link is ordinary. It exists only to end the server that accepts and then never speaks |
| `connect_timeout` | 30 s | Unchanged |
| `max_redirects` | 5 | GitHub Releases answers with a 302 to `objects.githubusercontent.com`, so redirects must work; 5 is far short of a loop |

`Content-Length` is checked before the body is read, and then **not trusted** —
the streaming counter enforces the same ceiling whether the header lies, or is
absent, or the body is chunked. Bodies land in a `.part` file promoted by rename
only on success, so a failed transfer cannot leave something at a
finished-looking name, and cannot destroy the artifact already there.

---

## 20. `--target-version`, because clap owns `--version`

**Decided:** 2026-08-13 · **Status:** active

`delta-release` declared `--version` for the release being built while
`#[command(version)]` declared clap's own. Clap catches the duplicate in a
`debug_assert`, so **every invocation of a debug build panicked** — including
`--help`.

The bug is trivial. How it survived is not.

- **The library tests never parsed arguments.** They call `build_release` and
  friends directly, because that is where the logic lives. Argument parsing is
  not library code, so a suite can be thorough about the logic and still never
  touch the boundary a user arrives at.
- **The CI check ran the binary in the profile that hides it.** `release.yml`
  ran `cargo run --release … -- --help`. Clap's uniqueness check is a *debug*
  assertion, compiled out under `--release`. The one step that did invoke the
  executable did so in the single configuration where the failure is invisible.

So the lesson is narrower than "test the CLI". It is: **run the real binary, in
the profile that checks the most.** `crates/delta-release/tests/cli.rs` does
that via `CARGO_BIN_EXE_delta-release`, in debug, over the whole flag surface the
release workflow depends on. All six of its tests fail if the collision returns.

**Revisit when:** never, for the rename. The testing rule generalises: any binary
this project ships gets a test that executes it.

---

## 21. A narrow, honestly-tested Tauri range beats a broad assumed one

**Decided:** 2026-08-13 · **Status:** active

`tauri-plugin-updater = "2"` accepted any 2.x. Every security-relevant claim this
plugin makes had been verified against **2.10.1** and nothing else.

Six behaviours are load-bearing, and not one is a guarantee of upstream's public
API — each is an observation about an implementation, free to change in a patch
release:

| Fact | Source | Depends on it |
| --- | --- | --- |
| `Update::install` verifies nothing | `updater.rs:718` | Why this plugin verifies at all (#10) |
| `verify_signature` called from one place | `updater.rs:712` | Same |
| The verifier's exact steps | `updater.rs:1453` | Our reproduction accepting what Tauri accepts (#6) |
| `raw_json` retains the fetched document | `updater.rs:492`, `:552` | The single-fetch identity (#13) |
| `get_urls` tries installer-specific keys first | `updater.rs:1420` | Binding delta metadata to Tauri's selection (#13) |
| `validate_endpoints`' http policy | `config.rs:145` | Mirroring it in `HttpFetch` (#19) |

A caret range let all six drift while CI stayed green — and worse, stayed green
*convincingly*, because the tests exercise our reproduction of a behaviour that
had moved. Green tests against a stale assumption are more dangerous than no
tests, because they are believed.

**Decided:** `tauri-plugin-updater = ">=2.10.1, <2.11.0"`, plus
`crates/plugin/tests/upstream_compat.rs`, which reads the resolved version out of
`Cargo.lock` and fails if it is not in a list of versions someone has actually
read. The failure message names the six sites to re-read. It is a prompt to do
the reading, not a number to bump.

`tauri` itself stays at `"2"`. No claim here rests on reading *its* source; the
security surface is entirely the updater plugin's, and constraining the app's own
framework version would be cost without evidence behind it.

The cost is real and accepted: consumers on a newer updater cannot use this
plugin until someone re-reads the source and widens the range. For an updater
that is the correct direction to be wrong in.

**Revisit when:** upstream offers a stable API for the two things we currently
read implementation details for — a verifying install handoff, and a way to learn
which platform key `get_urls` selected. Either would let a claim rest on a
contract instead of an observation, and the range could widen honestly.

---

## 22. The platform key is recovered, not reconstructed

**Decided:** 2026-08-13 · **Status:** active · **Corrects #13** · **Found by the real-app E2E**

Gate A bound the delta lookup to `identity.target()`, on the assumption that
`Update.target` is Tauri's `{os}-{arch}` platform key. **It is not.**

```rust
// updater.rs:403
let target = if let Some(target) = &self.target { target }
             else { updater_os().ok_or(Error::UnsupportedOs)? };
```

`Update.target` is the **OS alone** — `"darwin"` — unless the app configured an
override. The architecture lives in a separate `Update.arch` field, and the
candidate keys `{os}-{arch}-{installer}` and `{os}-{arch}` are built *locally
inside `get_urls`* and discarded; only the URL and signature are returned.

### Why it was misread

Two different things are called "target":

| | Value |
| --- | --- |
| `Update.target` — the **field** | `"darwin"` |
| `updater::target()` — the **free function** (`updater.rs:1313`) | `"darwin-aarch64"` |

Upstream's own endpoint templates treat them as separate variables:
`{{target}}/{{arch}}` expands to `/darwin/aarch64/`. Gate A read the field with
the function's semantics.

**How it survived four gates:** the symptom is silent. `patch_for("darwin", …)`
matched nothing, `plan_update` fell back with `reason: None`, and a full download
succeeded. Every test passed, CI was green, and users would have received correct
updates — just never a delta. A delta updater that never deltas looks exactly
like a working updater from the outside. Only the real-app E2E, which asserts
*which path ran*, could catch it.

### The rule

The key is **recovered from what Tauri kept**, not reconstructed:

> Find the platform entries whose `url` **and** `signature` both equal the ones
> Tauri selected.

| Case | Behaviour |
| --- | --- |
| Exactly one match | Use it |
| Zero | **Refuse** — `IdentityMismatch { field: "platform" }` |
| Several, agreeing on the target | Use it — duplicates with one answer |
| Several, disagreeing | **Refuse** — `AmbiguousPlatform` |
| Same URL, different signatures | Disambiguated by signature |
| Same signature, different URLs | Disambiguated by URL |

This is #13's principle applied where #13 itself failed to apply it: *consume
Tauri's decision, never re-derive it.* Matching on the pair is strictly stronger
than matching a key we compute, because it cannot drift from upstream's search
order — it never reproduces that order at all.

### One behaviour deliberately relaxed

Where the selected artifact has **no** delta entry, the outcome is now an
ordinary full download rather than a refusal. The pre-correction model refused,
which was the conservatism flagged when #13 landed. Publishing no delta for one
artifact is a normal release choice, not an attack.

**Revisit when:** upstream exposes which key `get_urls` selected. That would make
this a direct comparison rather than a recovery, and the ambiguity cases would
collapse.

---

## 23. The cache's mutable state needs a compare-and-set, not an atomic write

**Decided:** 2026-08-13 · **Status:** active

The artifact cache has two halves with different concurrency properties, and
conflating them would have cost either correctness or a lock nobody needs.

**Blobs** are content-addressed and immutable. Two processes that compute the
same artifact write the same bytes, so a race costs duplicated work and nothing
else. No coordination.

**State** — which entry is ACTIVE and which is PENDING — is mutable, and every
transition is a read-modify-write. Writing it atomically stops a *torn file*; it
does not stop a **lost update**:

```text
P1 reads {active: v1, pending: v2}   P2 reads {active: v1, pending: v2}
P1 promotes  -> {active: v2}
                                     P2 stages v3 -> {active: v1, pending: v3}
                                        ^ P1's promotion is gone
```

Both writes were atomic. The second was computed from state that had already
changed. **Measured, not argued**: 8 threads × 10 transitions against a plain
atomic replacement produced 16 surviving generations for 80 commits — 64 lost.
With the compare-and-set below: 80 commits, 80 generations.

### Why `hard_link`, not `rename`

`rename` is the obvious primitive and it is the wrong one:

| | destination exists |
| --- | --- |
| POSIX `rename(2)` | silently replaces — no CAS, lost updates return |
| Windows `MoveFileW` | fails |

Building on `rename` yields a design that happens to work on macOS and silently
loses updates on Windows. `hard_link` fails with `AlreadyExists` on **both**,
which is exactly the create-if-absent primitive a CAS needs.

State lives in `state.<generation>.json`; a writer that read *N* may only create
*N+1*; the loser of a race re-reads and retries.

### The race that is accepted

Reconciliation may discard a PENDING entry another process staged moments
earlier. Accepted, because the consequence is a cache miss and a full download —
never an incorrect ACTIVE and never an integrity failure. A global lock to
eliminate wasted work would be machinery for a threat this shape of application
does not have, which is the same argument as #18.

**Revisit when:** the cache is shared between processes that update
concurrently and often, rather than a desktop app whose trigger is a click.

---

## 24. Promotion asks what is *running*, not what was installed

**Decided:** 2026-08-13 · **Status:** active

The cache exists to hold the artifact the user is **currently running**, because
that is the only correct base for the next patch. So the state machine answers
"which version is running now?" — not "did the update succeed?".

Those are different questions:

| | Means |
| --- | --- |
| `Update::install()` returns `Ok` | bytes were written |
| the app relaunches as `v'` | `v'` is what is running |

Only the second licenses a promotion. Between them the user may never restart,
the launch may fail, an OS update may roll the bundle back, or another updater
may install something else. Promoting on `Ok` would leave ACTIVE naming an
artifact the user is not running — and the failure would be **silent**, because
the manifest's declared base digest would match the *cache* while the real
installation never received an update it could apply. The cache would be
confidently wrong forever.

So a verified artifact is staged as PENDING, and the next launch promotes it
from the version the running process reports about **itself**
(`package_info()`, compiled in) — never a value read from a manifest, a filename
or the cache.

```text
EMPTY ───────────── verified v' ─────────────▶ PENDING(v')
ACTIVE(v) ───────── verified v' ─────────────▶ ACTIVE(v) + PENDING(v')

next launch, running == v'   ─▶  ACTIVE(v')            (promote)
next launch, running != v'   ─▶  ACTIVE(v) unchanged   (discard PENDING)
```

**A failed install therefore needs no handling at all.** The next launch is
still the old version, so the same rule discards the staged entry. There is no
install-result signal to plumb through, which means there is no install-result
signal to get wrong — the best kind of error handling is the kind that is not a
special case.

### Staging happens *before* the install

The artifact is fully verified by that point, and staging first means a process
that dies mid-install still has the bytes it just paid for. It costs nothing in
correctness precisely because promotion asks a different question.

---

## 25. The tar layer is additive, not `schema: 2`

**Decided:** 2026-08-13 · **Status:** active

Patching the tar inside a `.app.tar.gz` instead of the compressed artifact is
worth 83.9% of the patch bytes on the controlled pair (#15, and F13 in the
research ledger). It is published as an **optional `tar_layer` block on the
existing platform entry**, not as a new schema version.

### Why not bump the schema

Because the capability is **per-client, not per-release**. Using the tar layer
requires rebuilding the exact compressed artifact, which a client either can do
or cannot. A schema bump would make every existing client refuse the whole
document — including its direct patches, which they can still use. That is the
opposite of what an additive optimisation should do.

So:

- existing `patches` keep exactly their current meaning;
- the compressed target's digest, size and signature stay on `DeltaPlatform`
  and are **not duplicated** into the tar layer — one fact, one place, per #13;
- target tar properties live on the layer, base properties on the per-version
  tar patch, because that is where each is needed;
- a client that does not implement the declared `representation` or
  `recompression` reads the direct patch instead.

### Unknown identifiers are *valid*, and that is the load-bearing part

`Manifest::validate` accepts a `tar_layer` naming identifiers this build has
never heard of. Rejecting them at parse time would take the platform's direct
patches down with a block the client merely cannot read — one bad forward
reference poisoning a path that works. Support is a client capability, so it is
decided at *selection* time by `TarLayer::support`, not at parse time.

What validation does reject is anything a well-formed document cannot contain
whatever the reader's capabilities: malformed digests, zero-sized declarations,
a patch from the release to itself, a base declared identical to the target.

**Incompatible future semantics get a new identifier**, never a new meaning for
an old one. `app-tar-gz-v1` and `tauri-app-tar-gz-v1` are frozen.

---

## 26. What `tauri-app-tar-gz-v1` actually pins

**Decided:** 2026-08-13 · **Status:** active · **Supersedes the negative result in the recompression probe**

The tar layer requires rebuilding the published `.app.tar.gz` byte-for-byte,
because minisign covers the compressed artifact and `Update::install` consumes
it (#6, #10). An earlier probe recorded this as **not achievable** for a shipped
plugin. Its measurements were right; its conclusion was not.

### What the probe got right, and where it stopped

Four independent DEFLATE backends produced an identical 4,070,510-byte
candidate against a 4,070,756-byte official artifact, differing 37 bytes into
the stream. Four implementations agreeing with each other and all falling short
implicates the **write pattern**, not the encoder. That inference was correct.

It then tested one-shot compression and stopped, recording the mechanism as a
hypothesis and naming exactly why it could not settle it: *"tauri-bundler is not
vendored here and was not read."*

### Reading it settled it

`tauri-bundler` does not compress a tar. It streams:

```text
tar::Builder  ->  flate2::write::GzEncoder  ->  File
```

so the encoder sees the archive as a sequence of writes whose boundaries are the
tar's own structure — a 512-byte header, the payload in `io::copy`-sized pieces,
one padding write, a 1024-byte trailer. Replaying that topology reproduces both
official artifacts from the controlled build **exactly**, and the signature
issued over the original verifies against the rebuild. Uniform 8192-byte
chunking of the same tar does not match, so it is the entry awareness that
works, not the chunk size.

This is the seventh time reading upstream replaced a plausible belief with a
certain one (F18). The gap between "four implementations disagree with the
official artifact" and "therefore it cannot be done in-process" was one unread
source file wide.

### What is contracted and what is merely observed

| Element | Status |
| --- | --- |
| The write topology | **Read** from `tauri-bundler` 2.8.1 and `tar` 0.4.x |
| `Compression::default()`, mtime 0, OS byte 255 | Read from the same source |
| 8192-byte payload chunks | `std::io::copy`'s `DEFAULT_BUF_SIZE` — a standard-library implementation detail |
| flate2's **zlib-rs** backend | **Observed.** The default `miniz_oxide` produces different bytes and can never match |
| GNU long-name entries | **Inferred** from `tar`'s source; no artifact here exercises them |

None of that is a guarantee anyone offers, and it does not have to be. **The
recipe is never trusted.** Its output is checked against the digest the release
published, and a mismatch falls back to a full download like any other delta
failure. The identifier exists so that a future incompatible recipe gets a new
name rather than silently producing wrong bytes under the old one.

`delta-release` runs the client's whole path once before publishing and refuses
to emit a tar layer it could not consume itself — because a release whose recipe
does not reproduce its own artifact would make every client do the work and fall
back, which is strictly worse than publishing no tar layer at all.

**Revisit when:** `tauri-bundler` changes how it writes the archive, or the
standard library changes `io::copy`'s buffer size. Either invalidates the
recipe, the fixture test goes red, and the correct response is a new identifier
rather than a fix under the old one.
