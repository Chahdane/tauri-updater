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

### F13 — Compression representation is what explains F11 vs F12 · **HYPOTHESIS**

The obvious reading: patching a compressed stream destroys the locality a delta
depends on, and patching the uncompressed tar layer restores it.

Well-motivated and untested here. Confirming it needs a controlled experiment:
identical inputs, one variable, provenance recorded, both representations
measured in the same run. Until that exists this is a hypothesis, and the
distance between F13 and F11/F12 is exactly the distance between an explanation
and a measurement.

**Research TODO.** Not scheduled; not required for correctness.

### F14 — Build reproducibility is a confounder · **HYPOTHESIS**

If two builds of unchanged source produce different bytes, some of every measured
delta is build noise rather than source change. Nothing here has measured that
noise floor, so no ratio in this project can currently be attributed cleanly.

Any future benchmark should establish the floor first: build the same commit
twice and diff.

### F15 — Per-platform, version-distance and change-category effects on ratio · **UNPROVEN**

Plausible and unmeasured. Listed so they are not quietly assumed.

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

### F19 — A real Tauri application installing a delta update · **UNPROVEN**

The project's largest open claim. No running app has ever received, verified,
reconstructed and installed a delta update. Every test to date uses a recording
handoff by design.

Nothing in Gates A–D substitutes for this, and no green CI run should be read as
implying it.
