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

*Status:* still true of the manifest, and **no longer decisive**. F27 binds the
release identity into bytes the key already covers, so the version a client
compares against is authenticated rather than asserted. The manifest itself
remains unsigned.

### F27 — The minisign trusted comment is an authenticated channel nobody was reading · **DEMONSTRATED**

A minisign block carries two Ed25519 signatures under the same key, the second
over `artifact_sig ‖ trusted_comment`. The comment is therefore bound to *that
specific artifact's signature*, and `PublicKey::verify` — the call
`tauri-plugin-updater` already makes — checks both halves. Release identity can
be authenticated at the cost of a parser: no second key, no second signature, no
second fetch.

Measured on the three tamper cases the design depends on. Editing the version
inside the comment: **rejected**. Splicing another release's own genuine comment
onto this artifact's signature: **rejected**. Swapping the whole signature block:
**rejected**.

Consequence beyond the obvious one: *absence* is authenticated too. Stripping the
identity from a bound release to force the unrestricted legacy path invalidates
the global signature, so "this release has no binding" is a property of the
release rather than a claim by the server — which is what makes a permissive
migration safe.

*Evidence:* `research/experiments/2026-08-14-minisign-trusted-comment-binding`;
mechanism read at `minisign-verify-0.2.5/src/lib.rs:334`; DECISIONS #27;
`crates/plugin/tests/release_identity_flow.rs`, which asserts the same properties
through the real flow on every CI run. Mutation evidence: removing the
delta-availability gate makes `a_legacy_signature_makes_a_published_delta_unavailable`
fail, and rewriting a contradiction into a full download makes
`an_authenticated_contradiction_never_becomes_a_full_download` fail.

### F28 — Authenticity, identity and freshness are three properties, and only two are held · **DEMONSTRATED**

Conflating them is how this class of bug survives review, so they are separated
explicitly:

| Property | Claim | Status |
| --- | --- | --- |
| Authenticity | these bytes were signed by the key | held, by minisign |
| Identity | these bytes *are* release X of app Y | held, by F27 |
| Freshness | release X is the newest published | **not held** |

An attacker controlling the server can still serve a *genuine older release
carrying its own genuine old identity*. Against an existing install the downgrade
policy refuses it, and that refusal is now trustworthy because the version it
reads is signed. Against a **first install** there is no floor, and nothing
establishes what "latest" means. A stale-but-genuine manifest is equally
undetectable: the manifest is unsigned and there is no expiry, timestamp
authority or role separation. This is **not** TUF-style freshness and must not be
reported as it.

*Evidence:* DECISIONS #27; case 4 of the binding probe, which is *accepted* by
verification and refused only by version policy.

### F29 — A decision applied twice from a paragraph naming three cases leaves the third · **DEMONSTRATED**

`docs/DECISIONS.md` #18 listed three shared filenames — `update.patch`,
`update.artifact`, `full.artifact` — argued that a per-update workspace beats a
lock, and implemented it for the first two. The third kept its fixed name in a
shared directory for four gates, in a decision record written specifically about
it.

The one left out was the **common** path. Delta paths are conditional; falling
back to a full download is what a cold cache, a legacy signature, a missing patch
or any delta failure does, so the isolated paths were the rare ones.

Restoring the shared path makes eight concurrent updates fail in two distinct
ways: seven lose their in-flight `.part` file to another transaction's rename,
and one reads a **different release's artifact** into its own signature check.
Not an integrity failure — `VerifiedArtifact` owns what it authenticated (F5) —
but #11 makes `Error::Signature` non-recoverable, so the race converts a routine
update into a permanent-looking abort.

The generalisation is about *form*, not about this bug: a rule stated in prose
and applied by hand at N call sites is a rule that holds at N−1 of them. The fix
extracted `transaction_workspace`, so the rule is now applied by being called.

*Evidence:* `crates/plugin/tests/transaction_isolation.rs`; DECISIONS #18, #28.
Mutation: restoring `work_dir.join("full.artifact")` fails all three tests.

### F30 — A guard written for one quantity does not extend to a pipeline added later · **DEMONSTRATED**

Gate B established `written ≤ declared ≤ local cap` and applied it to the
compressed installer, which was the only quantity that existed. The tar layer
then added four more stages, each sized by the same unauthenticated document.
Three inherited a cap incidentally, from the cache's own limits and the HTTP
ceiling. The reconstructed target tar inherited none — and its declared size is
not passive: it becomes zstd's window and output bound.

Removing the new check does not make a test fail at the check. It makes the
client reconstruct the tar and *then* report that the manifest declared 500 GB,
which is the resource attack having already completed.

Corollary that generalises: the caps must be **independent** dials, not one
number and a ratio. Every stage boundary here is a compression or patch
application, and both have unbounded expansion ratios, so a cap derived from an
earlier stage inherits exactly the unboundedness it was meant to remove.

*Evidence:* `crates/delta-core/src/limits.rs`; the size cases in
`crates/plugin/tests/tar_delta_flow.rs`; DECISIONS #19, #29.

### F31 — "It is already there" is a claim about a name, not about contents · **DEMONSTRATED**

`BlobStore::get` treats the cache as hostile on every read — kind, size, digest,
signature. `BlobStore::put` did not: it published with `hard_link` and read
`AlreadyExists` as "content addressing means those are the same bytes". True of a
file the store wrote; false of a *path*, which anything running as this user can
create.

A directory planted at a content address made staging report success, recording a
cache entry that named it.

The interesting part is the severity, because it is easy to overstate in both
directions. It is **not** unauthorised installation: reuse rejects the entry and
the update falls back, exactly as designed. It **is** availability — the bogus
entry is promoted over a good ACTIVE on the next launch, so the cache is
permanently unreadable and every future update pays full price, for the cost of
creating one directory.

*Evidence:* `a_blob_path_occupied_by_a_directory_is_a_clean_write_failure`;
DECISIONS #30.

### F32 — Two temporary-state collectors need different thresholds for the same reason · **ENGINEERING DECISION**

Orphaned cache blobs are collected after 60 seconds. Orphaned transaction
workspaces were not collected at all, so a crash mid-update stranded a full copy
of the artifact forever — inert, since the names are random and nothing
enumerates them, but unbounded.

Adding the sweep with the cache's threshold would have been the obvious move and
the wrong one. A blob is immutable and re-derivable, so collecting a live one
costs a re-download; a workspace holds a running transaction's state, so
collecting a live one breaks the update. The asymmetry sets the number: 24 hours,
above the slowest plausible download rather than above the typical one.

Which makes the load-bearing test the one nobody writes first — not that wreckage
is removed, but that a **live sibling survives** a concurrent update's sweep.

*Evidence:* `abandoned_workspaces_are_swept_and_live_ones_are_not`,
`creating_a_workspace_does_not_disturb_a_concurrent_one`; DECISIONS #31.

### F33 — A branch nothing executes is a branch that does not work · **DEMONSTRATED**

The release workflow *handled* the no-previous-release case: there is a
deliberate `::notice::` explaining it. What it actually did was skip the step
that produces the manifest, so a first release published a bare artifact with no
updater document and no signature, and the update system stayed inert until a
second release created one.

Every layer had a reason not to catch it. The type required two installers, so
the case could not be constructed. The crate's tests all had two installers,
because that is what the type demanded. The workflow's rehearsal ran
`delta-release --help`. Nothing anywhere had ever executed the branch.

The generalisation is not "test the first-release case". It is that **a
conditional whose two sides are exercised unequally is a conditional with an
untested side**, and the untested side is usually the rare one — which is exactly
the one nobody notices is broken. Compare F29: the same shape, one layer down.

*Evidence:* DECISIONS #32; `crates/delta-release/tests/release_states.rs`;
`examples/desktop-app/e2e/rehearse-release.sh`. Mutation: restoring the
requirement fails states A and B and leaves C green, which is the world B5
describes.

### F34 — Metadata about an artifact is not evidence about what it does · **DEMONSTRATED**

The release tool recorded a patch's digest and size and published metadata for
it, having never applied it. The digest proves a client downloads the patch that
was generated. It says nothing about what that patch produces.

The failure it permits is silent by construction: a delta that does not
reconstruct falls back to a full download, so no user reports it and every user
pays full price. The tar layer had round-tripped its output from the start; the
direct path had not, and the module docs said so plainly, which is its own
finding — a known gap written down and left.

Second-order result, and the more transferable one: **the guard was untestable
where it lived**. Inlined in the generator, the only reachable assertion was "the
honest case still works", and deleting the check failed no tests at all.
Extracting it as a function that takes a base, a patch and an expected digest is
what made a real adversarial case expressible — a patch that is genuine, correct,
and between the wrong pair.

*Evidence:* DECISIONS #33; `prove_patch_reconstructs` and the two tests that
drive it. Mutation before extraction: 0 tests failed. After: 2.

### F35 — Real transport is one cross-host hop, and the client already fit it · **DEMONSTRATED**

Measured against a public GitHub release, unauthenticated
(`research/experiments/2026-08-14-github-release-transport`):

```text
github.com/.../releases/download/...  ->  302
release-assets.githubusercontent.com  ->  200  (Azure Blob, Content-Length set)
```

One hop, cross-host, HTTPS throughout, and the CDN URL is signed with about an
hour's expiry.

Three client properties depend on that shape and all three already held: the
redirect budget covers it; the hop is cross-**host**, so any same-host policy
would have broken every real download, and the plugin deliberately has none; and
the chain never leaves TLS, so the no-downgrade rule is free on the happy path.

Worth recording as a finding rather than a footnote because the measurement
*corrected* something: the code documented the redirect target as
`objects.githubusercontent.com`. It is now `release-assets.githubusercontent.com`.
No code depended on it — nothing pins a host — but a comment stating a fact
nobody had checked recently is how a host-pinning "optimisation" gets written
later on a false premise.

*Evidence:* the probe log; `crates/plugin/src/http.rs`.

### F36 — Delta updates are Apple-compatible by construction, and unvalidated · **ENGINEERING DECISION**

`tauri-plugin-updater` 2.10.1's macOS installer (`updater.rs:1209`) takes
**bytes**, decompresses, and unpacks with the `tar` crate. Everything Gatekeeper
needs is inside those bytes, so preserving a code signature across an update is a
question about what the bundler wrote and what `tar` restores.

The delta path does not change that question. It reconstructs an artifact
bit-identical to the full download, proves it by digest before installing, and
hands the same `install_inner` the same bytes. So the tar layer is compatible
with a signed and notarized app **exactly to the degree the stock full-download
updater is**, and adds no new risk to the code signature.

That is an argument, and its premise — byte-exact reconstruction — is checked
rather than assumed. Two residual risks are specific and named: GNU long-name
entries, whose write pattern is inferred from `tar`'s source and never measured
(#26), and bundle shapes the recipe has not met, since the retained fixture is
unsigned. Neither can ship a broken update, because `--require-tar-layer` runs
the client's whole path at release time and fails the release instead.

**Classification is the point.** No notarized end-to-end has been performed,
because it needs a Developer ID certificate and an App Store Connect key that do
not exist here. That is an **external validation gap, not a software defect**:
nothing is known or suspected to be wrong, and no evidence has been gathered that
it is right. `spctl --assess` on a delta-updated app is the command that would
settle it.

*Evidence:* `docs/RELEASING.md`; `tauri-plugin-updater` 2.10.1 source, read.

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

*Evidence:* the three tests named above, each of which now fails when its guard
is removed. The mutation tables that recorded the original Gates A–C pass lived
in a working document that is no longer part of this repository, so the durable
evidence is the tests themselves rather than a transcript of the run.

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

### F37 — The managed delta path reaches a non-macOS client · **DEMONSTRATED at the engine and flow level**

Before this, a Windows or Linux application could not take a delta through the
shipping API, and nothing said so. Four things were linked, and the third is the
one that hid the other three.

| | What it did | Consequence |
| --- | --- | --- |
| 1 | `install_blocking` set the direct base to `None` outside `test-support` | nothing could supply a base |
| 2 | `plan_update` read the managed cache only for the tar path | the cache could not supply one either |
| 3 | `stage_pending` gunzipped every artifact to record a tar digest | an NSIS `.exe` could not be cached at all |
| 4 | the cache namespace hard-coded `app-tar-gz-v1` on every OS | a Windows cache claimed to hold macOS bundles |

Three is the interesting one: cache persistence is deliberately non-fatal, so the
install *succeeded*, a `CacheNotPersisted` diagnostic was returned to an
application that had no reason to treat it as anything but noise, and every
subsequent update stayed cache-cold. `Full, Full, Full, …` for ever, with
nothing red. That is `F19`'s failure mode — a delta updater that never deltas
looks exactly like one that works — arriving one layer down.

*Evidence:* `crates/plugin/tests/direct_delta_flow.rs` runs the whole ladder —
Full, promotion on relaunch, DirectDelta from the artifact Full staged — against
artifacts that are **not** gzipped tarballs, with the cache opened under
`opaque-v1` explicitly. It therefore runs on all three CI platforms rather than
only on Windows, so a macOS-hosted change that breaks the Windows route fails
before a Windows runner sees it. CI re-runs the ladder by name and fails if it
did not execute.

Every fallback case — cold cache, wrong base, undeclared base, corrupt patch,
truncated patch, missing patch, corrupt cached blob — asserts the direct path was
**reached** before it fell back, and the two that can be decided without spending
bytes assert that no patch was fetched.

**Scope, precisely.** This is engine and flow evidence with the network and the
installer faked. It says nothing about whether a real NSIS installer accepts the
bytes, whether the cache survives a real reinstall, or what a real patch ratio
is. Those are `F38`.

### F38 — A real Windows NSIS installation takes DirectDelta, but the opaque patch saves almost nothing · **DEMONSTRATED FOR WINDOWS X86_64 NSIS**

GitHub Actions [CI run 139](https://github.com/Chahdane/tauri-updater/actions/runs/35751082711),
commit `8a70543`, `windows-latest` x86_64,
tauri-cli 2.10.1, `tauri-plugin-updater` 2.10.1. Three distinct applications
were built as NSIS `-setup.exe` installers and one current-user installation
moved through both releases:

| | Transition 1 | Transition 2 |
| --- | --- | --- |
| Cache before | EMPTY | ACTIVE(1.0.1) |
| Required and observed | **Full** | **DirectDelta** |
| Network evidence | full installer fetched; patch absent | patch fetched; full installer absent |
| Patch / Full | 3,115,925 / 3,171,360 (**98.2520%**) | 3,116,863 / 3,172,306 (**98.2523%**) |
| Installed executable SHA-256 | `708cd5f0653604d809e8004c7e48cc00a6aff4123ae4e37da0562a077e862e35` | `d2c886c2759a83bafa7227e260a71724d52ce025cd4e6446772809ade5940a26` |

Both PENDING artifacts remained unlicensed until the updated application
relaunched and reported its own version. A separate degradation pass corrupted
the cached installer without changing its length; the next update selected Full
and installed the exact 1.0.2 executable. The engine-level fallback and
fail-closed matrix remains in F37; this run adds what it could not: a real NSIS
handoff, cache survival across real reinstalls, selected-path/request-log proof,
and installed-file equality.

The retained artifact provenance is:

| Version | Installer bytes | Installer SHA-256 | Main executable SHA-256 |
| --- | ---: | --- | --- |
| 1.0.0 | 3,172,730 | `7c6f9a07370fe5296ff3a0a67d5ab69e3c432c7817aca403e636c9c818814775` | `b35f8e11ec0756a24ad7aa5adffa240097fe10d56c03a5c6e3cd72de7456c5ad` |
| 1.0.1 | 3,171,360 | `94b8a39b6142dbccea031c034b7cf8912d9ded5258032210309d87b7eee1aee5` | `708cd5f0653604d809e8004c7e48cc00a6aff4123ae4e37da0562a077e862e35` |
| 1.0.2 | 3,172,306 | `de35f5e313c7d821c872dc6a581c148501b6bd82daac7ad8ff27bfb8beca6434` | `d2c886c2759a83bafa7227e260a71724d52ce025cd4e6446772809ade5940a26` |

The first transition binds base BLAKE3
`da916deb6613fa62698cac6e94bc1a2e0d9b17bddc409dbb0d7dcd7b045569e7`
to target BLAKE3
`d5c84772edb6d2da0ba705649624f8f3a3bb7ca1793ad52b228ac2296f6e8d7b`.
That target is the second transition's base; its target is
`397c1420e24ba688533eb41107f41f72785401ffcac2b6d4a4802f61560296d9`.

**The ratio is the result.** NSIS uses solid LZMA compression, so a small source
change moved almost the whole opaque installer. Windows support is therefore a
correctness and availability claim, not a bandwidth claim. An inner
representation is future research, not something this evidence permits the
project to promise.

**Scope.** Unsigned NSIS, Windows x86_64, GitHub-hosted runner, loopback plain
HTTP enabled only in the E2E build, controlled version-string changes. MSI,
ARM64, Authenticode/SmartScreen and public HTTPS remain unproven.

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

**Rerun under authenticated identity**
(`research/experiments/2026-08-14-macos-authenticated-identity-e2e`): rebuilt from
scratch with every release signed under F27, the same two transitions produce the
same two outcomes — Full, then a 641,941-byte tar delta against 4,175,538. That
the second one is a tar delta *is itself* the proof the authenticated path ran: a
legacy binding makes both delta paths unavailable by construction, so no delta
outcome is reachable without a parsed, verified, matching identity. The identity
check did not close the fast path it protects.

**Rerun under runtime hardening**
(`research/experiments/2026-08-14-macos-runtime-hardening-e2e`): rebuilt from
scratch after the full path moved into a per-transaction workspace, the tar
pipeline gained a ceiling on every stage, the blob store stopped inferring a blob
from an occupied path, and abandoned workspaces became collectable. Same two
outcomes, and a 640,153-byte tar delta against 4,177,582.

What that is evidence *for* is narrow and worth stating: the hardening did not
close the fast path it protects. It is **not** independent evidence for the
guards themselves. A happy-path E2E cannot exercise a ceiling nothing approaches
or a race that does not occur, so those rest on the unit and flow tests and on
five mutations — which is the right division of labour, not a gap.

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

`delta-release` applies every generated direct patch and requires the target
digest before writing its metadata. For each tar-layer patch it additionally
recompresses the reconstructed tar and requires byte-identity with the artifact
it was given.

The failure it catches is invisible otherwise: if the recompression recipe does
not reproduce *this release's* artifact — a property of the bundler's dependency
graph, not of the tool — every client would do the work and fall back, and the
manifest would look entirely correct. `refuses_to_publish_when_recompression_would_not_reproduce_the_artifact`
exercises it against a genuinely-compressed artifact the recipe cannot rebuild.

Both releases of the three-version build round-tripped against artifacts
`cargo tauri build` had just produced, which is independent of the committed
fixture: it shows the recipe works on the current toolchain, not only on one
artifact retained from an earlier run.

Closes Audit #2 blocker **B7 for both paths**. The multi-predecessor release test
also re-applies both generated direct patches independently, so adding a second
base does not reduce this to a one-patch assertion.

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

### F39 — A first update can rebuild its tar base from the installed bundle · **DEMONSTRATED ON FIXTURES; UNPROVEN ON A REAL INSTALL**

With nothing cached, the tar path repeats `tauri-bundler`'s `tar::Builder`
call over the installed `.app` and uses the result only when the tar and its
`tauri-app-tar-gz-v1` recompression match the base the patch declares
(DECISIONS #37).

*Demonstrated, on fixtures:* an untouched bundle reproduces the bundler's
exact tar; a modified bundle, a missing bundle and an oversized rebuild are
each rejected, the first two before any patch is downloaded; an empty cache
plus an untouched bundle installs the published bytes via TarDelta without
fetching the full artifact; and a usable cache is preferred to the bundle.
*Evidence:* `crates/delta-core/src/recompress.rs` tests,
`crates/plugin/tests/tar_delta_flow.rs` (`a_first_update_rebuilds_its_base_…`
and the four cases beside it).

*Not demonstrated:* that a real application installed from a DMG still matches
its published tar. Tar headers carry mtime, uid, gid and mode and follow
`read_dir` order, so the answer depends on how the installation copied the
bundle. An app installed by Tauri's own updater is known **not** to match
(directory mtimes are reset by per-entry unpacking), which is harmless because
that installation already has a cache. The settling experiment: build two
releases, install the first by mounting its DMG and copying the `.app` with
Finder, run the update against an empty cache, and require the request log to
show the tar patch fetched and the full artifact not fetched.

### F40 — `compression: "none"` makes Windows direct patches ~5% of Full, at 4× the Full size · **STRONG OBSERVATION**

CI run [36025510400](https://github.com/Chahdane/tauri-updater/actions/runs/36025510400)
(`main`, commit `793de7c`, `windows-latest`, tauri-cli 2.10.1), the NSIS E2E job,
with the example app built by `cargo tauri build` and published by
`delta-release`:

| Pair | Full installer | Direct patch | Patch / Full |
| --- | ---: | ---: | ---: |
| 1.0.0 → 1.0.1 | 12,658,708 | 640,768 | 5.0619% |
| 1.0.1 → 1.0.2 | 12,658,708 | 630,200 | 4.9784% |

Against F38's solid-LZMA builds of the same app (Full 3,171,360; patch
3,115,925; 98.25%): each delta update downloads 4.9× fewer bytes, and each Full
download is 4.0× larger.

A strong observation rather than a demonstration of a typical ratio: the builds
differ only in their version string, which is the smallest possible release,
and the two settings were measured in different runs. The controlled
comparison, with a feature-sized change and zlib and per-file LZMA added, is
`examples/desktop-app/e2e/measure-nsis-compression.sh`, run manually by the
`NSIS compression study` workflow. It has not been run yet. Options and
trade-offs: [NSIS_COMPRESSION.md](NSIS_COMPRESSION.md).

### F41 — tauri-plugin-updater 2.11.0 keeps all six load-bearing behaviours; 2.12.0 does not · **DEMONSTRATED by source reading**

The published 2.11.0 and 2.12.0 crates were read against 2.10.1 at each of the
six sites in `crates/plugin/tests/upstream_compat.rs`. 2.11.0: three function
bodies byte-identical, the verifier's steps unchanged (return type only),
`install` still unverified, `raw_json` still retained, `Update`'s public fields
identical. 2.12.0: the verifier additionally parses the minisign trusted
comment for a tab-separated `version:` field (`signed_version`,
`verify_signed_version`), with an opt-in `requireSignedVersion`. This
project's `delta-v1` identity has no such field (DECISIONS #39).

*Since shown:* the plugin's whole test suite passes against 2.11.0 in the
`upstream updater / range-max` CI job, on every PR since #36.

### F42 — Patch sizes for an app with a realistic payload · **DEMONSTRATED for the benchmark app**

`examples/desktop-app/e2e/benchmark.sh`, CI run
[36181326029](https://github.com/Chahdane/tauri-updater/actions/runs/36181326029)
on `main` at `55acaf2` (after DECISIONS #42 and #43), tauri-cli 2.10.1
`--locked`. The app bundles ~88 MiB of seeded resources: 32 × 2 MiB of
incompressible "media" and 24 × 1 MiB of JSON. 1.0.1 changes only the version;
1.0.2 replaces two media files, adds one, edits ~2% of the lines in five JSON
files, deletes one, and changes the frontend. "Download" is what the plugin
fetches: the tar patch on macOS, the direct patch on Windows.

| Platform | Change | Full | Download | / Full | bsdiff (reference) |
| --- | --- | ---: | ---: | ---: | ---: |
| Windows NSIS (`compression: "none"`) | version only | 104,993,535 | 558,917 | 0.53% | 172,936 |
| | feature change | 106,042,145 | 7,045,600 | 6.64% | 6,596,232 |
| | two releases behind | 106,042,145 | 7,049,183 | 6.65% | 6,594,810 |
| macOS `.app.tar.gz` | version only | 76,132,814 | 636,564 | 0.84% | 388,110 |
| | feature change | 78,027,384 | 6,875,621 | 8.81% | 6,638,274 |
| | two releases behind | 78,027,384 | 6,995,737 | 8.97% | 6,726,740 |

About 6 MiB of each feature-change patch is the replaced and added media,
which no diff can shrink; that is the floor these numbers sit near. Two
releases behind costs almost exactly one release, because each predecessor
gets its own direct-to-current patch (item 1). The macOS direct patch
(19% of Full) is not what clients download; it is published only as the
fallback when the tar layer cannot be used.

*Scope:* one synthetic but realistically shaped payload, one change shape, GitHub
runners. A real application's ratio depends on how much of it changes. Before
DECISIONS #43 the Windows feature-change patch was 31.4% of Full (F43), over
the publish limit.

### F43 — zstd needs match tables sized to the reference for large installers · **DEMONSTRATED**

On the benchmark's real Windows installers (105 MB, `compression: "none"`,
CI run [36168262483](https://github.com/Chahdane/tauri-updater/actions/runs/36168262483)),
for the feature-change pair:

| Setting | Patch | / Full | Time |
| --- | ---: | ---: | ---: |
| level 19, full window, LDM (as shipped in 0.1.0) | 33,260,824 | 31.38% | 18.5 s |
| same, window = old + new | 33,259,613 | 31.38% | 18.3 s |
| same, hash and chain logs = window | 7,266,609 | 6.86% | 19.7 s |
| bsdiff4 | 6,971,853 | 6.58% | ~50 s |

The window was never the problem; the match finder's tables were. Fixed in
DECISIONS #43. On macOS's inner tar the same change moved 6.78% to 6.64%, so the
effect is specific to how much of a large reference the default tables miss.

### F44 — The tar-layer recipe depends on the bundler's compressor build · **DEMONSTRATED**

tauri-cli 2.10.1 installed with `--locked` bundles with flate2 1.1.1 /
zlib-rs 0.5.0 / tar 0.4.43; installed without it, it resolved flate2 1.1.9 /
zlib-rs 0.6.7 in August 2026. The two produce different gzip bytes: the
plugin, then on 0.6.7, could not rebuild the benchmark app's `--locked`
artifact, and with 0.5.0 it cannot rebuild the earlier fixture. Pinning the
plugin to the `--locked` versions made the macOS benchmark's
`--require-tar-layer` pass (run
[36175835119](https://github.com/Chahdane/tauri-updater/actions/runs/36175835119)),
and the regenerated fixture's signature verifies against its rebuild.
DECISIONS #42.

### F45 — NSIS compression settings, measured together · **DEMONSTRATED for the example app**

`measure-nsis-compression.sh`, CI run
[36167683814](https://github.com/Chahdane/tauri-updater/actions/runs/36167683814),
example app, 1.0.0 → 1.0.1 with a rewritten page and a new 64 KB script:

| Setting | Full | Direct patch | / Full |
| --- | ---: | ---: | ---: |
| LZMA, solid (Tauri default) | 3,176,862 | 3,124,018 | 98.34% |
| zlib, solid | 4,382,085 | 3,991,491 | 91.09% |
| LZMA, per file (custom template) | 3,180,712 | 3,051,593 | 95.94% |
| none | 12,671,508 | 314,741 | 2.48% |

Per-file compression does not help because the frontend is embedded in the
executable, which changes every release. A zstd-19 copy of the uncompressed
installer is 3,432,555 bytes, 8% more than solid LZMA; that is what DECISIONS
#40 publishes for Full downloads.

### F46 — The first Windows update is a delta from an installer kept at install time · **DEMONSTRATED ON A REAL WINDOWS X86_64 NSIS INSTALL**

CI run [36168949264](https://github.com/Chahdane/tauri-updater/actions/runs/36168949264),
`windows nsis e2e`: a real current-user NSIS install of 1.0.0 with an empty
cache selected **DirectDelta** for 1.0.0 → 1.0.1, fetched the patch and not the
installer, and installed the exact 1.0.1 executable. The installer hook had
kept the 1.0.0 installer, and after the updater's silent install the kept
file was the 1.0.1 installer. With the cache corrupted and the seed removed,
the next update degraded to Full. DECISIONS #41.
