# Project audit and continuation handoff

**Audit date:** 2026-09-22

**Audience:** project owner, Claude, Codex, and future maintainers

**Repository:** `Chahdane/tauri-updater`

**Audited checkout:** `main` at `e0267fb45a28f1b22487263f04a2e88a38dbe0ad`

**Worktree at audit start:** clean and equal to `origin/main`

This document records what the project is, what is genuinely proven, where work
stopped, what is currently broken or incomplete, and the safest order in which
to continue. It is a state/readiness audit, not a claim that the pending formal
P5 independent security review has been completed.

## Executive conclusion

The repository is a serious, unusually well-documented **macOS v0.1 release
candidate**, not a released cross-platform updater.

The strongest completed path is:

```text
macOS aarch64
1.0.0 -> Full -> relaunch/promote cache -> 1.0.1 -> TarDelta -> 1.0.2
```

That path has real-app evidence through Tauri's real updater and installer, with
the selected source and installed bytes asserted. The core engine, release
generator, signature/identity model, fallback rules, transport bounds, cache
state machine, and macOS tar recompression have extensive automated coverage.

The project stopped just before publication:

1. `main` was hardened into the macOS v0.1 release candidate on 2026-08-14.
2. A one-commit branch, `release/v0.1.0-metadata` at `35de1c6`, fixed crates.io
   packaging on 2026-08-18 and passed CI, but was never merged and has no open
   pull request.
3. No Git tag, GitHub Release, or crates.io package was published.
4. The only currently open pull request is Dependabot PR
   [#31](https://github.com/Chahdane/tauri-updater/pull/31). Its CI is red on
   Linux, macOS, and Windows, and its updater-range widening violates an
   intentional compatibility/security guard. Do not merge it as generated.

Moving to Windows is a new product phase. The byte-level engine is portable,
but the public runtime is still structurally macOS-specific. A normal Windows
application cannot currently take a delta path through the shipping API.

## Release-readiness verdict

| Area | State | Verdict |
| --- | --- | --- |
| Core diff/apply and verification | Cross-platform tests and deterministic fixture evidence | Strong |
| macOS `.app.tar.gz` client | Real aarch64 Full then TarDelta E2E | Demonstrated for controlled cases |
| Public Rust API | Implemented and merged; opaque same-`Update` binding | Ready for final review |
| Release generation | Full-only and delta states implemented; patches self-round-trip | Strong, with release-version issue below |
| Security model | Thoughtful fail-closed identity/signature boundary and bounded resources | Strong design; P5 still pending |
| GitHub-hosted HTTPS E2E | Harness exists; no credential-backed run | Unproven |
| Apple Developer ID/notarized E2E | Design argument only; no credentials/run | Unproven |
| crates.io publication | Main is not packageable for two crates; fix exists only on an unmerged branch | Blocked |
| Windows client | Engine compiles/tests in CI, but no usable managed delta path or real installer E2E | Not supported |
| Dependency security | One current Rustls vulnerability plus transitive advisories | Needs action before release |

## Exact repository and remote state

### Local/main

- Branch: `main`
- HEAD and `origin/main`: `e0267fb` — merge of PR #27, the release URL policy fix.
- Worktree was clean before this audit file was added.
- There are no tags.
- GitHub reports no releases.
- The three intended public crates return HTTP 404 from the crates.io API:
  `tauri-plugin-updater-delta`, `tauri-updater-delta-core`, and
  `tauri-updater-delta-release`.

### Last meaningful branches

| Ref | Commit | Meaning | Action |
| --- | --- | --- | --- |
| `origin/release/v0.1.0-metadata` | `35de1c6` | Adds versioned internal dependencies and crate README metadata; changes changelog to a dated 0.1.0 release | Reuse selectively; do not merge the stale release date verbatim |
| `origin/dependabot/cargo/cargo-109b59ec45` / PR #31 | `0af1710` | Updates seven dependency groups and widens `tauri-plugin-updater` to `<2.12.0` | Red CI; split and review, do not merge wholesale |
| `origin/main` | `e0267fb` | macOS v0.1 RC plus URL-policy hardening | Current authority |

Older feature/fix branches are retained remotely, but their work is already in
`main`. They are history, not unfinished alternatives.

### CI evidence

- Main CI run on `e0267fb` succeeded on 2026-08-14.
- `release/v0.1.0-metadata` CI succeeded on 2026-08-18.
- PR #31 lint and MSRV jobs succeed, but the test job fails on all three
  platforms. At minimum, the intentional tests reject:
  - resolving `tauri-plugin-updater` 2.11.0 when only 2.10.1 is in the
    human-reviewed set; and
  - widening the manifest range while README/tests still promise `<2.11.0`.
- Historical Dependabot PRs #28–#30 also failed tests and are closed.

Useful links:

- Main CI: <https://github.com/Chahdane/tauri-updater/actions/runs/31813863714>
- Metadata-branch CI: <https://github.com/Chahdane/tauri-updater/actions/runs/32162659265>
- PR #31 CI: <https://github.com/Chahdane/tauri-updater/actions/runs/34269023011>

## What the repository contains

| Location | Responsibility | Audit observation |
| --- | --- | --- |
| `crates/delta-core` | Hashing, zstd diff/apply, manifests, release identity, signature verification, cache, limits, macOS recompression | Platform-neutral core plus a macOS-specific cache/recompression layer |
| `crates/delta-release` | `delta-release`, `release-check`, signing, direct and tar patch generation | Generic opaque direct patches already work at release time |
| `crates/plugin` | Public Tauri API, HTTP transport, planning/fallback, verified install handoff | Shipping runtime is safe by default but currently reaches deltas only through the macOS tar cache |
| `crates/fixtures` | Deterministic shared test fixtures | Not published by design |
| `examples/desktop-app` | Minimal real Tauri application and macOS E2E harness | Configured for macOS `app` bundles, not Windows installers |
| `docs` | Architecture, decisions, releasing, roadmap | High-quality rationale; roadmap truthfully says Windows is later work |
| `research` | Immutable experiment records and findings ledger | Strong provenance for macOS claims; some older findings are intentionally historical |
| `.github/workflows/ci.yml` | lint, docs, MSRV, three-OS test matrix | Good load-bearing assertion checks |
| `.github/workflows/release.yml` | macOS aarch64 example-app build/sign/upload | Not a crate-publishing or Windows workflow |

Approximate code/test scale at audit time:

- 30 production Rust files, about 13,168 lines.
- 17 dedicated Rust test files, about 7,655 lines.
- 358 `#[test]` declarations in `crates/`, plus compile-fail doctests.
- All three library crates forbid unsafe Rust and warn on missing docs.

## Architecture and security state

### The sound parts

The central design remains good: reconstruct the exact official installer,
verify it, and hand it to Tauri's official installer instead of patching an
installed application in place.

Important properties enforced in code and tests include:

- one authoritative Tauri update check; no second manifest fetch;
- an opaque checked `Update` owns both identity and install handoff;
- final artifact signature verification occurs inside this plugin because the
  pinned upstream `Update::install` does not verify;
- authenticated `delta-v1` identity binds app id, version, platform,
  representation, digest, and size to signed artifact bytes;
- downgrade and authenticated contradictions fail closed;
- ordinary delta/cache/transport failures fall back to Full;
- legacy signatures remain Full-only;
- patches are untrusted and reconstructed output is size- and digest-checked;
- per-stage local ceilings bound downloads, patch output, gzip expansion, and
  tar reconstruction;
- per-update random workspaces prevent concurrent updates sharing filenames;
- cached blobs are content-addressed, immutable, re-hashed, and
  re-signature-verified before reuse;
- PENDING becomes ACTIVE only when a later launch reports the staged version;
- credentials/headers are sent only to Tauri's authoritative full-artifact URL,
  never to unauthenticated patch URLs;
- HTTPS is the production default and HTTPS-to-HTTP redirects are refused;
- no telemetry is introduced.

### Explicit limits of the model

- The manifest is not signed.
- Artifact authenticity and release identity are covered, but release
  freshness is not. This is not TUF.
- A first install has no local version against which to reject a genuinely
  signed old release.
- The strongest compatibility evidence is pinned to
  `tauri-plugin-updater` 2.10.1.
- macOS x86_64 is expected, not demonstrated.
- GitHub-hosted HTTPS and notarized macOS E2Es remain external validation gaps.

These are documented limitations, not hidden defects.

## What is genuinely complete on macOS

- Real macOS aarch64 install path demonstrated through public API and Tauri's
  real installer.
- First transition takes Full and stages PENDING.
- Relaunch promotes only the actually running version.
- Second transition takes TarDelta.
- Server request logs prove the full artifact/direct patch were not fetched on
  the tar-delta transition.
- Exact installed binary hashes are asserted.
- Corrupt/missing/truncated delta and wrong-base cases fall back to verified
  Full.
- Tampered final artifacts fail signature verification and install nothing.
- Controlled tar-layer patch ratios were roughly 15–16% of Full; the repository
  correctly avoids generalizing this result to arbitrary applications.
- Release generator applies every generated direct/tar patch before publishing
  metadata.
- macOS recompression is checked against a retained real artifact and signature.

## Windows audit: current reality

### What already helps Windows

- `delta-core` is byte-oriented and compiled/tested on Windows CI.
- zstd direct patch generation/application is platform-neutral.
- `delta-release` identifies non-`.app.tar.gz` artifacts as `opaque-v1` and can
  generate/directly verify a patch between two Windows installer files.
- Tauri's own `Update::install(bytes)` remains the intended Windows install
  handoff.
- Cache state publication uses a hard-link compare-and-set design explicitly
  reasoned about for Windows semantics.
- The example includes an `.ico`, and the Rust executable has the correct
  `windows_subsystem` attribute.

This is useful foundation, but it is not Windows client support.

### Why a Windows delta is currently unreachable

There are four linked structural blockers:

1. In the normal build, `Update::install_blocking` sets the direct-patch base to
   `None`. A direct base is supplied only by the non-default `test-support`
   feature.
2. `plan_update` can use the managed cache only for `tar_patch`; the direct
   patch path reads `ctx.base`, not the cache's ACTIVE artifact.
3. `ArtifactCache::stage_pending` always decompresses a verified artifact as
   gzip and records tar digest/size. An NSIS `.exe` or MSI cannot be persisted by
   that cache. Installation can still proceed because cache persistence is
   intentionally non-fatal, but every later update remains cache-cold.
4. `RuntimeConfig` hard-codes the cache namespace to
   `app-tar-gz-v1` / `tauri-app-tar-gz-v1` on every operating system.

Therefore the current expected Windows behavior is:

```text
check succeeds -> Full download -> verify -> cache staging fails as non-gzip
               -> diagnostic returned -> Tauri may install Full
next release   -> still no usable base -> Full again
```

No real Windows install has established even that Full path end to end, so it
must remain a hypothesis until tested.

### Windows release/harness gaps

- `examples/desktop-app/tauri.conf.json` has `bundle.targets: ["app"]`; it does
  not request `nsis` or `msi`.
- The release workflow runs only on `macos-latest`, fixes the platform to
  `darwin-aarch64`, and searches only for `.app.tar.gz` assets.
- All E2E scripts are Bash/macOS-oriented.
- There is no Windows three-version build ladder, local HTTP controller run, or
  GitHub-hosted installer E2E.
- There is no real NSIS/MSI installed-file hash assertion or relaunch proof.
- There is no Authenticode-signed Windows validation.
- Patch efficiency for compressed NSIS/MSI installers is unmeasured. The
  macOS direct-compressed result (~95% of Full) warns that an opaque Windows
  direct patch may be correct but economically useless.

Tauri's current v2 documentation says updater artifacts are the normal NSIS
`-setup.exe` and MSI files when `createUpdaterArtifacts` is `true`; both are
valid future targets. Start with one, preferably NSIS, to keep the first proof
bounded: <https://v2.tauri.app/plugin/updater/>.

## Findings requiring action

### A1 — Blocker: Windows managed delta path does not exist

The release side can make an opaque patch, but the shipping client cannot retain
or supply an opaque ACTIVE artifact to the direct patch planner. Fixing this is
the first Windows implementation milestone. Do not describe Windows as
supported until a real Full -> relaunch -> DirectDelta ladder passes.

### A2 — Blocker: release tag/version tracks are conflated

The workspace/crates are version `0.1.0`, while the example application is
version `1.0.0` in both `Cargo.toml` and `tauri.conf.json`.

The release workflow derives `--target-version` from the Git tag but does not
change or verify the version compiled into the example app. As a result:

- tagging `v0.1.0` would build an app whose own version is `1.0.0`, then sign and
  publish it as release identity `0.1.0`;
- tagging `v1.0.0` would match the example app but not the plugin/crate release;
- the documented hosted rehearsal tags `v0.0.0-hosted-a/b` cannot represent the
  unchanged example app version correctly.

`release-check` compares tag, manifest, signature identity, and bytes, but it
does not inspect the version compiled inside the app bundle, so it will not
catch this mismatch.

Before any tag, separate these release tracks or add a build-time invariant that
the compiled app version equals the release target. A crate-release tag should
not accidentally trigger publication of a differently versioned demo app.

### A3 — Blocker: `main` cannot package the dependent public crates

Audit commands produced:

```text
cargo package -p tauri-updater-delta-core --no-verify   -> packages
cargo package -p tauri-updater-delta-release --no-verify -> fails
cargo package -p tauri-plugin-updater-delta --no-verify  -> fails
```

Both failures say `tauri-updater-delta-core` has no version requirement. The
unmerged metadata branch fixes this with versioned path dependencies and adds
README metadata. Its changelog date, however, claims a release on 2026-08-18
that never occurred. Reuse the manifest fixes but keep the release under
`[Unreleased]` until it actually happens, or set the date at publication time.

### A4 — High: locked Rustls version has a current security advisory

An OSV batch query over all 522 unique locked registry package/version pairs
found `rustls 0.23.43` affected by
[RUSTSEC-2026-0285](https://osv.dev/vulnerability/RUSTSEC-2026-0285), fixed in
`0.23.45`. It is in both the plugin's Reqwest 0.12 transport and upstream
updater's Reqwest 0.13 transport.

The advisory says a peer can make Rustls accept TLS 1.3 handshake messages at
the wrong encryption level. The transcript remains authenticated, so this is
not described as a network attacker forging a completed handshake, but an
updater on a security boundary should not knowingly ship the affected lock.

Update Rustls within the existing compatible dependency graph, then rerun the
full cross-platform and transport suites before release.

The same scan found:

- `glib 0.18.5` / RUSTSEC-2024-0429, fixed in 0.20.0. This is in the Linux GTK
  target graph, not the audited Windows/macOS runtime path.
- Six unmaintained-package warnings: `proc-macro-error` and five `unic-*`
  crates, all transitive through GTK/Tauri or `urlpattern`/`tauri-utils`.

Track these upstream; do not confuse the unmaintained warnings with the Rustls
runtime vulnerability.

### A5 — High: Dependabot PR #31 must not be merged wholesale

PR #31 changes more than routine patch versions:

- zstd 0.13 -> 0.14;
- minisign 0.7 -> 0.9;
- base64 0.22 -> 0.23;
- flate2 1.1.9 -> 1.1.10;
- `tauri-plugin-updater` lock 2.10.1 -> 2.11.0;
- plugin requirement `<2.11.0` -> `<2.12.0`.

The last item directly contradicts Decision #21. Six security-relevant upstream
implementation behaviors were reviewed only against updater 2.10.1. The red
test is doing its job.

Handle updates in small groups. For updater 2.11.x, re-read and record the six
upstream sites listed in `crates/plugin/tests/upstream_compat.rs`, update the
verified set/docs only after that review, and rerun real E2E. For flate2/zstd,
also re-run exact patch digest and macOS recompression assertions because output
determinism is part of the protocol.

### A6 — Medium: `release-check --allow-insecure-urls` is broader than stated

The generator's opt-in restricts HTTP to loopback. The independent checker,
however, accepts every `http://` URL whenever its flag is true, even though its
CLI says the flag is for loopback rehearsals only. This does not weaken the
normal production workflow, which never passes the flag, but the checker and
generator policies can disagree in the very mode intended for E2E.

Use one shared URL parser/policy for generator and checker and add a checker test
that rejects non-loopback HTTP even with the flag. The generator's hand-rolled
host split also fails to recognize documented IPv6 `[::1]`; a real URL parser
would fix both issues.

### A7 — Medium: publication is still manual and incomplete

- No crates.io publish workflow or documented dependency-order publish command
  exists.
- The GitHub Release workflow publishes the example application, not the three
  crates.
- No tag or release exists.
- The README still correctly says the plugin and tool are unpublished.

Define the publication order (`core`, `release`, then `plugin`), dry-run each
package from an extracted package/clean external consumer, reserve/verify names,
and make the crate/GitHub release relationship explicit.

### A8 — Environment limitation: native tests cannot run on this Windows host

The audit downloaded the locked dependencies and started:

```text
cargo test --workspace --all-features --locked -- --nocapture
```

Windows Application Control blocked Cargo's generated Serde build-script
executable with OS error 4551 before project tests executed. WSL enumeration
was also access-denied by policy. This is a machine policy failure, not a test
failure.

`cargo fmt --all --check`, `git diff --check`, Cargo metadata/tree inspection,
and packaging checks do run. Use the GitHub Windows runner or obtain an approved
local build-output location/policy exception before relying on this machine for
native Rust validation.

## Verification performed during this audit

| Check | Result |
| --- | --- |
| Git status and history | Clean at start; main equals origin/main |
| Live remote heads/tags/PR refs | Queried directly; no tags; PR #31 is the only open merge ref |
| GitHub API: PR/releases/actions | One open PR, zero releases; CI states recorded above |
| crates.io API | All three intended package names absent (404) |
| Rust/Cargo | `rustc 1.98.1`, `cargo 1.98.1`, Windows MSVC; declared MSRV is 1.88 |
| Formatting | `cargo fmt --all --check -v` passed |
| Whitespace | `git diff --check` passed |
| Full local tests | Blocked before execution by Windows Application Control, OS error 4551 |
| Main CI | Previously green on Linux, macOS, Windows at the audited commit |
| Package construction | Core packages; release/plugin fail on unversioned internal dependency |
| Dependency inventory | Lock resolves updater 2.10.1, Tauri 2.11.5, zstd 0.13.3, flate2 1.1.9 |
| Vulnerability query | 522 locked registry packages queried via OSV; Rustls finding plus target/transitive warnings recorded above |
| TODO/FIXME scan | No meaningful implementation TODO/FIXME markers; unfinished work is tracked in roadmap/docs/branches instead |

Not performed here:

- local Clippy/docs/tests, because they require executing blocked build scripts;
- a real Windows installer build/install/relaunch;
- macOS-only E2E reruns from Windows;
- credential-backed GitHub/Apple/Windows code-signing validations;
- destructive publication, tags, secrets use, or release creation.

## Recommended continuation plan

### Phase 0 — restore a releasable baseline

1. Update locked Rustls to a non-affected compatible version and run all CI.
2. Fix the example-app/tag/crate-version ambiguity. Add an automated invariant.
3. Fix the checker/generator loopback-policy mismatch.
4. Bring the package metadata changes from `release/v0.1.0-metadata` onto a new
   branch, without preserving the false 2026-08-18 release date.
5. Run `cargo package`/`cargo publish --dry-run` for all public crates in clean
   external consumers and document publication order.
6. Decide whether macOS v0.1 ships before Windows work or whether the project
   remains unreleased until Windows. Do not let one release tag mean both the
   crate and the differently versioned demo app by accident.

Suggested branch sequence:

```text
fix/rustls-advisory
fix/release-version-contract
fix/release-check-loopback
chore/release-metadata
feat/windows-opaque-cache
feat/windows-nsis-e2e
```

### Phase 1 — make the cache representation-aware

Preserve the macOS tar path while adding an opaque artifact mode:

1. Derive the cache namespace representation from the platform/artifact mode;
   do not hard-code macOS identifiers on Windows.
2. Make tar digest/size metadata representation-specific or optional.
3. Let `stage_pending` store a verified opaque installer without gzip
   decompression.
4. Reuse `ArtifactCache::active()` as the normal direct-patch base for an
   `opaque-v1` release.
5. Keep the signature re-verification, PENDING/ACTIVE promotion, content-address
   rules, size limits, and namespace/key isolation unchanged.
6. Add tests proving macOS entries cannot be read as opaque Windows entries and
   vice versa.

Do not obtain the base by reading the installed `.exe`; cache the exact official
updater installer, because only that file is guaranteed to match the release
patch base.

### Phase 2 — measure Windows before promising savings

Start with NSIS only:

1. Change a Windows-specific example configuration to build `nsis` with
   `createUpdaterArtifacts: true`.
2. Build the same source twice to measure the reproducibility/noise floor.
3. Build three distinct versions with a small controlled source change.
4. Generate and round-trip opaque direct patches for both transitions.
5. Record exact installer hashes, sizes, patch hashes, ratios, toolchain, and
   provenance under `research/`.

If an NSIS direct patch is close to Full, treat that as a result, not a failed
test. Investigate an inner deterministic representation only after the
measurement justifies the complexity. Do not claim Windows support merely
because an inefficient direct patch is byte-correct.

### Phase 3 — real Windows update proof

The minimum credible Windows E2E is:

```text
install 1.0.0
  -> update to 1.0.1 via Full
  -> confirm exact installed executable/version
  -> relaunch and observe cache promotion
  -> update to 1.0.2 via DirectDelta
  -> prove patch was fetched and Full was not
  -> confirm exact installed executable/version after relaunch
```

Also exercise:

- corrupt, truncated, missing, oversized, and wrong-base patches -> verified
  Full fallback;
- tampered Full/final artifact -> install nothing;
- concurrent update attempts -> isolated workspaces;
- install modes and restart/exit behavior for the chosen installer;
- user-level paths with spaces and non-ASCII characters;
- locked files/antivirus behavior;
- cache-unavailable diagnostics;
- HTTPS and redirect behavior on the real hosted path.

### Phase 4 — Windows release support

- Add a Windows release job and explicit artifact discovery for the chosen
  installer.
- Generate `windows-x86_64` manifest entries and patch assets.
- Keep updater minisign distinct from Authenticode.
- Add an external Authenticode-signed E2E or document it as a credential-bound
  gap exactly as the Apple gap is documented.
- Update README/ROADMAP/support tests only after the real install evidence
  exists.
- Add MSI only as a separate demonstrated target; do not infer it from NSIS.

## Windows acceptance criteria

Windows support may be claimed only when all of the following are true:

- The default, non-`test-support` public API reaches a Windows delta.
- A Full update persists an opaque PENDING artifact without a cache diagnostic.
- Relaunch promotes only the version actually running.
- A later update selects DirectDelta from that ACTIVE artifact.
- The reconstructed installer matches the published BLAKE3 and minisign
  signature and carries matching authenticated identity.
- The real Tauri Windows installer accepts the reconstructed bytes.
- The harness asserts the selected outcome and request log, not just final
  installed bytes.
- Fallback and fail-closed cases remain distinct.
- The test runs on a Windows runner with the chosen NSIS/MSI format.
- Patch ratio and provenance are recorded without universal savings claims.
- Documentation and compatibility tests name the exact demonstrated platform,
  architecture, installer type, Tauri CLI, and updater version.

## Rules future agents must preserve

- Do not merge PR #31 or widen the updater range just to make Dependabot green.
- Do not assume `Update.target` is the manifest platform key; recover Tauri's
  selected entry from authoritative URL plus signature as current code does.
- Do not assume `Update::install` verifies signatures.
- Do not fetch a second manifest.
- Do not turn authenticated contradictions, downgrade refusals, or signature
  failures into Full fallback.
- Do not forward full-artifact headers to patch URLs.
- Do not promote cache state merely because `install()` returned `Ok`.
- Do not use fixed shared transaction filenames.
- Do not patch installed binaries in place.
- Do not weaken resource ceilings based on server-provided sizes.
- Do not remove the release generator's patch self-round-trip.
- Do not resolve a `docs/DECISIONS.md` conflict by taking one whole side; preserve
  and renumber both decisions and update every reference.
- Do not use an empty-password Tauri signing key with this tooling.
- Do not claim universal patch savings from controlled fixtures.

## Best next task

The safest immediate implementation task is **not** the Windows E2E yet. First
create a small release-baseline PR that updates Rustls, fixes the tag/app version
invariant, fixes checker loopback parity, and restores packageability. Once that
is green, begin `feat/windows-opaque-cache` with tests that make the managed
ACTIVE artifact available to the direct patch path while leaving all macOS tar
tests unchanged.

That is the clean boundary between finishing the existing release candidate and
starting Windows support.
