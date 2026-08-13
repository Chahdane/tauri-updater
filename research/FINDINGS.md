# Findings ledger

Every claim this project might eventually make, with what actually backs it.

Seeded from work already done. **Nothing here was invented for the ledger**, and
nothing was promoted a level to look better. Several entries exist specifically to
record that a plausible-sounding claim is *not* supported.

## Classifications

| Class | Requires |
| --- | --- |
| **DEMONSTRATED** | Reproducible evidence in this repository. A test, a CI run, or an experiment record with full provenance. Someone else can re-run it and get the same answer. |
| **STRONG OBSERVATION** | Measured, but once, or without controls, or without provenance sufficient to attribute a cause. Probably true. Not established. |
| **HYPOTHESIS** | A proposed explanation with no controlled evidence yet. May be well-motivated. Is not a result. |
| **ENGINEERING DECISION** | A choice with stated reasoning and trade-offs. Correctness is not the kind of thing that applies; it can be well- or badly-argued. |
| **UNPROVEN** | Asserted somewhere, or assumed, and not backed. Listed so it stops being invisible. |

---

## Correctness of the engine

### F1 — Patch round-trip reproduces the target exactly · **DEMONSTRATED**

Applying a generated patch to the base yields bytes identical to the target,
checked by BLAKE3. Covered by the round-trip suite and by real-transport tests
that compare installed bytes against the released artifact.

*Evidence:* `crates/delta-core/tests/appimage_roundtrip.rs`,
`crates/plugin/tests/update_flow.rs`, `crates/plugin/tests/real_transport.rs`.

### F2 — Patch generation is byte-identical across Linux, macOS and Windows · **DEMONSTRATED, for controlled fixtures**

The same fixture pair produces a patch with digest
`6892d72ab49da606bbeb98c04c8110aa145038492fbf849568590b99d8d5b546` on all three
platforms. CI re-runs the assertion by name and fails if it did not execute, so a
green job carries the claim rather than merely permitting it.

**Scope, precisely:** synthetic fixtures, one zstd configuration. It says nothing
about determinism of *bundler output*, which is a separate and unaddressed
question (see F10).

*Evidence:* `.github/workflows/ci.yml`, determinism step, all three platforms.

---

## Security architecture

### F3 — Tauri verifies on download, not on install · **DEMONSTRATED**

`tauri-plugin-updater` 2.10.1 calls `verify_signature` from exactly one place,
inside `Update::download()` (`updater.rs:712`). `Update::install(bytes)` goes
straight to `install_inner` with no verification (`updater.rs:718`).

The delta path exists to avoid that download, so **this plugin's own check is the
only one on the path**. Established by reading upstream source, not inferred from
behaviour.

*Evidence:* DECISIONS #10; `crates/delta-release/tests/tauri_signature_compat.rs`
pins the verifier equivalence.

### F4 — Artifact authenticity is not manifest authenticity · **DEMONSTRATED**

A minisign signature proves the *bytes* were signed by the expected key. It
covers no manifest field — not the version, URLs, sizes or digests — because the
release document is not signed at all.

Consequence, and the reason this is a finding rather than a definition: an
attacker who can rewrite the manifest, **with no signing key**, can serve a
genuinely signed *older* release. Every cryptographic check passes. Only version
policy refuses it.

*Evidence:* DECISIONS #11, #13.
`replaying_an_old_genuinely_signed_release_installs_nothing`, plus mutation
evidence — disabling the downgrade guard makes that test fail with
`got Ok(InstalledFromFullDownload)`, i.e. the attack succeeding.

### F5 — A capability type can make an unverified install unrepresentable · **DEMONSTRATED**

`VerifiedArtifact` has a private field and no public constructor, so
`verify_artifact` is the only way in the language to obtain one, and the install
handoff takes `&VerifiedArtifact`. Forgery does not fail review; it fails to
compile.

The doctests proving this are themselves guarded: CI counts them and fails if it
does not see exactly two `compile fail ... ok`.

*Evidence:* `crates/delta-core/src/signature.rs`; CI forgery-doctest step.

### F6 — Owning the verified bytes closes a TOCTOU window · **ENGINEERING DECISION**

The token holds the bytes it authenticated rather than a path to them, so what
was verified and what is installed are the same allocation. A path would leave a
window in which the file could be swapped.

Classified as a decision, not a demonstration: no test exercises a concurrent
swap between check and handoff. The argument is structural and the cost —
keeping the artifact resident — is the same one Tauri's own `download()` pays.

*Evidence:* DECISIONS #10; `verifies_from_a_file` shows a post-verification
overwrite cannot affect the token.

### F7 — A single authoritative response removes the update-identity seam · **DEMONSTRATED**

Two independent fetches of the release document allowed one answer to Tauri's
semver gate and a different answer to the delta planner. Reading the delta layer
out of `Update::raw_json` — which upstream retains verbatim — means there is no
second answer to disagree with.

*Evidence:* DECISIONS #13. Ten regression cases, and mutation runs killing 3, 2,
8 and 2 tests as each guard was disabled.

---

## Testing methodology

### F8 — Library tests do not substitute for boundary tests · **DEMONSTRATED**

`delta-release` shipped with a clap argument collision that made **every
invocation of a debug build panic, including `--help`**. A full library suite
passed throughout, because argument parsing is not library code.

Worse, the one CI step that *did* run the binary ran it under `--release`, where
clap's duplicate detection is a debug assertion and compiled out — the single
configuration in which the bug is invisible.

The generalisation is narrower than "test the CLI": **run the real binary in the
profile that checks the most.**

*Evidence:* DECISIONS #20; `crates/delta-release/tests/cli.rs`, all six tests of
which fail if the collision is reintroduced.

### F9 — A guard is not proven load-bearing until its test fails without it · **DEMONSTRATED**

Mutation testing each security guard — disable it, confirm the intended test goes
red for the intended reason, restore — found **three of six transport tests
could not detect their own guard's removal**:

- The HTTPS refusal was only asserted in release builds, where the suite never
  runs.
- The redirect test used a self-loop, so removing the budget made it *hang*
  rather than fail. A test that hangs under mutation is not evidence.
- The partial-file test asserted only "nothing left behind", which is equally
  true of writing to the destination and deleting it on error.

Without the mutation pass, three guards would have shipped with tests incapable
of detecting their removal. This is the strongest methodological result the
project has.

*Evidence:* `SPRINT.md` mutation tables for Gates A–C.

### F10 — Fake transports hide socket-level failure modes · **STRONG OBSERVATION**

A `HashMap`-backed `Fetch` cannot express a server that sends headers and then
stalls, a redirect chain, a lying `Content-Length`, or an endless chunked body.
Each required a real socket to test, and each found behaviour the fake could not
have.

Observation rather than demonstration: nobody has enumerated which real-world
failures a fake transport *does* catch, so the strength of the claim is not
quantified.

*Evidence:* `crates/plugin/tests/real_transport.rs`, `tests/support/server.rs`.

---

## Delta efficiency

### F11 — One macOS `.app.tar.gz` pair produced a patch ≈95% of the target · **STRONG OBSERVATION**

Measured on a real example-app bundle pair, patching the compressed
`.app.tar.gz`.

**This is not evidence that gzip causes it.** The audit that produced the number
also found the executables differed substantially, timestamps and metadata
differed, provenance was not controlled, and deterministic recompression was
never demonstrated. Any of those could account for part of the gap.

Recorded as: *one observed macOS archive pair produced a ~95% compressed-artifact
delta.* Not as: *gzip makes Tauri delta updates 95%.*

*Evidence:* DECISIONS #15. No experiment record — provenance was not captured at
the time, which is precisely why `experiments/` now exists.

### F12 — A related tar-layer experiment produced ≈6.6% · **STRONG OBSERVATION**

Same caveats, same missing provenance. The two numbers differ by more than an
order of magnitude and are *related* experiments, not a controlled pair.

### F13 — Compression representation is what explains F11 vs F12 · **DEMONSTRATED, for one controlled pair** · *was HYPOTHESIS*

The reading was: patching a compressed stream destroys the locality a delta
depends on, and patching the uncompressed tar layer restores it.

This entry asked for a controlled experiment — *identical inputs, one variable,
provenance recorded, both representations measured in the same run* — and
`2026-08-13-macos-controlled-tar-layer` is that experiment. The **same** macOS
artifact pair, patched two ways:

| Representation | Patch | As % of the 4,070,756-byte compressed target |
| --- | ---: | ---: |
| `.app.tar.gz` (compressed) | 3,884,546 | **95.43%** |
| tar (uncompressed, inside it) | 625,269 | **15.36%** |

83.9% fewer patch bytes, with the representation as the only variable. The
reconstructed tar was byte-identical to the official one.

**Upgraded, not rewritten.** The hypothesis is now demonstrated *for this pair*
— one platform, one bundle format, one string-only source change. F14's build-
noise confounder is untouched, and nothing here generalises the magnitude to
other pairs. What is settled is the *direction and the mechanism*, which is what
F13 actually claimed.

### F14 — Build reproducibility is a confounder · **HYPOTHESIS**

If two builds of unchanged source produce different bytes, some of every measured
delta is build noise rather than source change. Nothing here has measured that
noise floor, so no ratio in this project can currently be attributed cleanly.

Any future benchmark should establish the floor first: build the same commit
twice and diff.

### F15 — Per-platform, version-distance and change-category effects on ratio · **UNPROVEN**

Plausible and unmeasured. Listed so they are not quietly assumed.

### F22 — A shipped plugin can rebuild the exact published `.app.tar.gz` in-process · **DEMONSTRATED**

Experiment `2026-08-13-macos-entry-aware-recompression`.

The tar layer is worthless unless the client can put the gzip layer back
byte-for-byte, because the minisign signature covers the compressed artifact and
`Update::install` consumes it (F3, DECISIONS #6). Replaying `tar::Builder`'s
write topology into `flate2`'s `GzEncoder` does exactly that, on **two**
independent official artifacts from the controlled build:

| Artifact | Rebuilt | Official | |
| --- | ---: | ---: | --- |
| v1.0.1 | 4,070,756 | 4,070,756 | identical |
| v1.0.0 | 4,070,693 | 4,070,693 | identical |

and the minisign signature issued over the original v1.0.1 artifact verifies
against the rebuild.

Reproducible from this repository alone:
`cargo test -p tauri-updater-delta-core --test macos_recompression`, against the
official artifact committed as a fixture.

**Two elements are observed rather than contracted.** The 8192-byte payload
chunks are `std::io::copy`'s internal buffer size, and the recipe requires
flate2's **zlib-rs** backend — its default `miniz_oxide` produces 3,936,113 bytes
for every topology and can never match. Neither is a guarantee anyone offers,
which is why the recipe carries a version identifier and why its output is
always gated by the published digest.

### F23 — The earlier negative recompression result was true of its recipe, not of the problem · **DEMONSTRATED** · *supersedes a conclusion, not a measurement*

`2026-08-13-macos-inprocess-recompression-probe` concluded that a shipped plugin
could not do this, and is **retained unchanged**. Every number in it reproduces
exactly: one-shot compression of the exact tar still yields 4,070,510 bytes
differing at byte 47.

Its measurements were right. Its inference — that four backends agreeing
implicated the *write pattern* — was also right. The error was in what it
compared the write pattern against: it tested one-shot and uniform chunking,
and `tauri-bundler` does neither. It streams `tar::Builder` **into** the encoder,
so the write boundaries are the tar's own entry structure. Uniform 8192-byte
chunking of the same tar produces 4,070,674 bytes and also fails.

The probe stated the mechanism as a HYPOTHESIS and named exactly why it could not
settle it: *"tauri-bundler is not vendored here and was not read."* Reading it
settled it. That is the same pattern as F18, arriving for the seventh time, and
it is the reason this record is worth keeping rather than deleting: the gap
between "four implementations disagree with the official" and "therefore it
cannot be done in-process" is one unread source file wide.

---

## Operational

### F16 — Empty-password Tauri signing keys are not interoperable · **DEMONSTRATED**

`tauri signer generate --password ""` produces a key this tooling cannot read.
Tauri's CLI uses rsign2, which runs its KDF even over an empty password; the
`minisign` crate skips encryption entirely when the password is empty. The
failure surfaces as "Wrong password for that key" about a key the user never gave
a password to.

*Evidence:* DECISIONS #16;
`a_key_that_will_not_open_without_a_password_explains_rsign2`.

### F17 — Availability failures and integrity failures need different budgets · **ENGINEERING DECISION**

Concurrent updates sharing fixed filenames could corrupt each other. Because
`VerifiedArtifact` owns its bytes, the worst outcome is a *loud failure*, never a
bad install — so the exposure is availability, not integrity.

That distinction chose the mechanism: a per-update workspace rather than
cross-process locking, on the grounds that the realistic trigger is a user
double-clicking a button.

*Evidence:* DECISIONS #18, including the condition that would reopen it.

### F18 — Reading upstream source repeatedly replaced belief with certainty · **STRONG OBSERVATION**

Six times, a plausible assumption was replaced by a fact found by reading
`~/.cargo/registry`: the `PublicKey::decode` encoding; `install()` not verifying;
the double-fetch trust seam; `raw_json` being retained; `tauri-build` requiring a
Windows `.ico`; and `validate_endpoints` already refusing plain HTTP in release
builds — which meant the E2E harness could never have run as configured.

Observation, not a demonstrated methodology result: six is a tally from one
project, with no comparison against how often the assumption would have been
right. The pattern is worth naming and is not a measurement.

### F19 — A real Tauri application installing a delta update · **DEMONSTRATED ON MACOS FOR THE RECORDED CONTROLLED E2E CASE**

Experiment `2026-08-13-macos-real-app-e2e-delta`, macOS 26.5.2 arm64.

A real running Tauri app performed a real `Updater::check()`, received 1.0.1 from
the served superset manifest, derived `UpdateIdentity` from that same `Update`
via `delta_identity()`, **selected the delta path**, downloaded the patch,
reconstructed the target, verified digest and signature, produced a
`VerifiedArtifact`, reached the real `Update::install()`, and left the installed
main binary **byte-identical to the expected 1.0.1 binary**:

```
old        6890b2e5724e5791fc0e081189eed5671685ffb06452715722c453ef0d7801e5
expected   6d299c03876815f2b437b729599171336b855815ea21f22592ff79ac334ab13e
installed  6d299c03876815f2b437b729599171336b855815ea21f22592ff79ac334ab13e
```

The runtime reported `installed-from-delta downloaded=3884546 full=4070756`.
That string is constructible only from `UpdateSource::Delta`, which
`attempt_delta` returns only after the patch downloaded, reconstruction ran, and
the rebuilt bytes matched the manifest digest — so reconstruction is **evidenced**
rather than inferred from the install succeeding. No fake handoff at any point.

**Scope, exactly.** macOS only, one architecture, one bundle format, one version
pair, plain-HTTP loopback, ad-hoc code signing. It demonstrates *integration*,
not production readiness, and closes none of the Audit #2 blockers. Windows,
Linux, rollback resistance and cross-platform E2E remain unproven.

**Preceded by a failure that was worth having.** The first run
(`2026-08-13-macos-real-app-e2e`, retained) reached real `Update::install` with
the correct bytes but selected **Full**, because `Update.target` is
`updater_os()` alone. Four gates of green tests had not caught it: a delta
updater that never deltas behaves exactly like a working updater unless something
asserts *which path ran*. See DECISIONS #22.

### F21 — Delta failure falls back to a verified full download, in a real app · **DEMONSTRATED ON MACOS FOR THE RECORDED CASE**

With the delta path genuinely attempted first, corrupt, truncated and missing
patches and a wrong base artifact each fell back and still installed bytes
byte-identical to the expected release. Tampered full artifacts were refused with
signature failures and installed nothing.

These same scenarios were **vacuous** in the first run — they passed while the
delta path was never attempted, so they proved nothing. They count now only
because F19 establishes the delta path actually runs.

Does **not** address Audit #2 blocker B4: the full-fallback workspace race is
untouched, and a single-threaded harness cannot speak to it.

### F24 — The cache-backed tar path, in a real app, across two real transitions · **DEMONSTRATED ON MACOS FOR THE RECORDED CONTROLLED RUN**

Experiment `2026-08-13-macos-cache-backed-tar-e2e`, macOS 26.5.2 arm64.

Three versions built by `cargo tauri build`, main binaries asserted pairwise
distinct. One installation moved through the real `Update::install` twice:

| | Transition 1 | Transition 2 |
| --- | --- | --- |
| Cache before | EMPTY | ACTIVE(1.0.1) |
| **Required** | **Full** | **TarDelta** |
| Observed | `installed-from-full-download` | `installed-from-tar-delta` |
| Downloaded | 4,163,366 | **633,594** |
| Installed binary | `573f3838…` = v1.0.1 | `7caff690…` = v1.0.2 |

**Which path ran is asserted, not inferred**, and so is what was *not* fetched:
the server's request log shows the tar patch downloaded and neither the full
artifact nor the direct patch. Both transitions install identical published
bytes, so a hash-only harness would have passed twice on an updater that
downloaded everything — which is precisely what happened in F19's first run.

**The chain is provable rather than assumed.** `base_tar_blake3` of the
1.0.1→1.0.2 patch equals `target_tar_blake3` of the 1.0.0→1.0.1 release, so the
artifact transition 2 patched is the one transition 1 cached.

Promotion is separately evidenced: after each install the cache held
PENDING with ACTIVE unchanged, and only the *relaunch* — the app reporting its
own version — promoted it.

**Scope.** macOS, one architecture, one bundle format, one ladder, string-only
source changes, loopback plain HTTP, ad-hoc signing. Integration evidence.
Closes no Audit #2 blocker except B7, and that only for the tar path.

### F25 — The tar layer's saving reproduces on independent pairs · **DEMONSTRATED, for three controlled macOS builds**

| Pair | Direct patch | Tar patch | Fewer patch bytes |
| --- | ---: | ---: | ---: |
| 1.0.0 → 1.0.1 | 3,963,466 (95.20%) | 626,006 (**15.04%**) | 84.21% |
| 1.0.1 → 1.0.2 | 4,023,061 (96.55%) | 633,594 (**15.21%**) | 84.25% |

Two pairs from one fresh build, with full provenance, reproducing the earlier
controlled single-pair result (F13: 15.36% vs 95.43%). The agreement across
three independent pairs is what lifts F13's mechanism claim from "one pair" to
"reproduced" — though still on one platform, one bundle format, and one class of
source change (F15 remains unproven).

### F26 — Release tooling that round-trips its own patch catches a failure nothing else can · **DEMONSTRATED**

`delta-release` applies its own tar patch, recompresses, and requires
byte-identity with the artifact it was given, before writing any metadata.

The failure it catches is invisible otherwise: if the recompression recipe does
not reproduce *this release's* artifact — a property of the bundler's dependency
graph, not of the tool — every client would do the work and fall back, and the
manifest would look entirely correct. `refuses_to_publish_when_recompression_would_not_reproduce_the_artifact`
exercises it against a genuinely-compressed artifact the recipe cannot rebuild.

Both releases of the three-version build round-tripped against artifacts
`cargo tauri build` had just produced, which is independent of the committed
fixture: it shows the recipe works on the current toolchain, not only on one
artifact retained from an earlier run.

Closes Audit #2 blocker **B7 for the tar path only**. The direct-patch generator
still does not round-trip its output.

### F20 — The macOS `.app.tar.gz` ratio, measured with full provenance · **STRONG OBSERVATION**

The same run measured a patch of 3,878,953 bytes against a 4,072,303-byte target
— **95.25%** — for two real bundles built by the actual Tauri pipeline from a
string-only source change.

This is the first ratio in the project with complete provenance (commit,
toolchain, artifact hashes, build command), and it is consistent with F11.

Still a **strong observation**, not a demonstration of the *cause*: it is one
pair, on one platform, patching the compressed representation, and no controlled
comparison against the uncompressed tar layer was run. F13 remains a hypothesis.
Note also that no client consumed this patch in the run — it was measured at
generation time, because the client took the full path.
