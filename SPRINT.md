# Sprint 4 — The real-app end-to-end

**Phase:** 3 of 8, final gate ([roadmap](docs/ROADMAP.md))
**Started:** 2026-08-12
**Status:** Paused for hardening — see below

---

# POST-CODEX HARDENING AUDIT

**Started:** 2026-08-12
**Status:** Gate A complete, awaiting review. Gates B–D not started.

An independent adversarial audit (Codex, given the repository cold and told to
challenge the architecture rather than agree with it) found the prototype
architecture sound but surfaced several gaps. Real-app E2E work is paused on the
preserved `feat/e2e-real-app` branch until the HIGH findings are resolved.

Every finding is verified against this repository and against upstream
`tauri-plugin-updater` source before anything is changed. Codex proposing a fix
is not a reason to implement that fix.

## Verification status

| # | Finding | Status | Gate |
| --- | --- | --- | --- |
| 1 | Double-fetch / update identity seam | **CONFIRMED** | A |
| 2 | Signature-failure reasoning overclaims | **CONFIRMED** (wording only; policy stands) | A |
| 3 | One coherent update identity | Design proposed | A |
| 4 | Downgrade / replay / version policy | **CONFIRMED** | A |
| 5 | HTTP transport hardening | **CONFIRMED** (all 8 sub-claims) | B |
| 6 | Resource exhaustion caps | **CONFIRMED** | B |
| 7 | Atomic file handling | **PARTIALLY CONFIRMED** | B |
| 8 | Concurrent update protection | **CONFIRMED** | B |
| 9 | `delta-release --version` collision | **CONFIRMED** — worse than reported | C |
| 10 | Release workflow assumptions | **CONFIRMED** | C |
| 11 | Tauri compatibility range | **CONFIRMED** | C |
| 12 | Documentation contradictions | **CONFIRMED** (A–E) | D |
| 13 | Plugin DX — design only | Deferred to Phase 5 proposal | — |
| 15 | macOS 95% vs tar 6.6% | Preserve as observation, do not overclaim | D |

## Gate A — trust architecture ✅

- [x] 1. Update identity: single authoritative response — `UpdateIdentity`
      carries Tauri's `Update`, and `run_update` has no manifest URL left to
      fetch
- [x] 2. DECISIONS #11 reasoning corrected; the fail-closed policy is unchanged
- [x] 3. `checked == delta == verified == installed` — structural for the first
      three (one parse of one document), closed by `VerifiedArtifact` for the
      fourth
- [x] 4. Downgrade / replay / version policy in `identity.rs`, applying to every
      caller rather than to whoever consulted Tauri's gate

Also fixed in Gate A: the platform-selection divergence found during
verification. Tauri searches `{os}-{arch}-{installer}` before `{os}-{arch}` and
does not report which key won, so the delta entry is now bound to the signature
Tauri selected rather than to a key recomputed here.

### Gate A regression tests

All ten requested cases are covered, plus the platform-selection binding:

| Case | Test | Where |
| --- | --- | --- |
| 1. checked version != delta target | `refuses_when_the_checked_version_is_not_the_delta_target`, `a_version_tauri_never_checked_installs_nothing` | core, plugin |
| 2. selected signature != delta signature | `refuses_when_the_delta_signature_is_not_the_one_tauri_selected` | core |
| 3. selected URL authoritative | `the_full_path_always_uses_the_url_tauri_selected` | core |
| 4. current > target | `refuses_a_downgrade`, `a_downgrade_never_becomes_a_full_download` | core |
| 5. replay of an old signed artifact | `replaying_an_old_genuinely_signed_release_installs_nothing` + `the_replayed_artifact_really_does_verify` | plugin |
| 6. malformed version | `refuses_an_uncomparable_version`, `refuses_an_uncomparable_target_version` | core |
| 7. prerelease ordering | `prereleases_order_by_semver` | core |
| 8. multiple versions behind | `a_target_several_versions_ahead_still_uses_its_patch` | core |
| 9. missing delta, legitimate target | `falls_back_when_the_installed_version_has_no_patch` | core |
| 10. no second manifest fetch | `never_fetches_the_manifest`, `the_manifest_is_never_fetched`, `the_flow_never_requests_the_manifest` | core, plugin, real HTTP |
| platform selection | `stays_bound_to_the_target_tauri_selected` | core |

### Mutation evidence

Each guard was disabled in turn, the intended tests confirmed red for the
intended reason, then restored. No mutations committed.

| Guard disabled | Tests killed | Sample failure |
| --- | --- | --- |
| version identity | 3 | `expected a refusal, got Delta { … }` |
| signature identity | 2 | — |
| downgrade refusal | 8 | `a replayed older release must be refused, got Ok(InstalledFromFullDownload)` |
| uncomparable-version refusal | 2 | — |

## Gate B — transport and resource safety

- [ ] 5. HTTPS policy, redirect policy, timeouts, response-size limits
- [ ] 6. Local safety cap independent of manifest-declared size
- [ ] 7. Atomic output via unique temp path then promote
- [ ] 8. Per-transaction workspaces replacing fixed filenames

## Gate C — concrete defects

- [ ] 9. Rename the CLI's `--version` to `--target-version`, test the real binary
- [ ] 10. Release workflow: make external assumptions explicit or supply them
- [ ] 11. Honest Tauri compatibility range

## Gate D — truthfulness

- [ ] 12. Documentation audit against current code
- [ ] 16. `research/` evidence structure and findings ledger, no fabricated data

## Preserved work

`feat/e2e-real-app` carries the example desktop app, the two-version build
script, the E2E harness scripts and the Codex audit transcript. Pushed to origin
as a backup on 2026-08-12; not to be reset, deleted or merged. It stacks on
`main` at 34a79db.

Note that its `examples/desktop-app/src/update.rs` still calls the old
two-fetch `run_update(manifest_url, …)` API and will not compile against this
branch. Porting it to `UpdateExt::delta_identity` is the first task when real-app
E2E resumes, and is itself a useful check that the new API is usable.

---

## Goal

Close the one claim still open: **a running Tauri app accepts a served update.**

Everything to date proves the algorithm. Nothing proves that a real
`Update`, obtained from a real `Updater::check()` inside a real app, installs a
delta-rebuilt artifact. 103 tests use a fake handoff — by design, and that is
exactly why none of them substitutes for this.

## Why an app is unavoidable

`tauri_plugin_updater::Update` has private fields (`run_on_main_thread`,
`config`, `extract_path`, `app_name`, `installer_args`). No test can construct
one. `Updater::check()` inside a running app is the only source, so the
control-surface route is **forced, not chosen**.

The counterweight: macOS `install_inner` is pure file manipulation — gzip, tar
extract, rename the live bundle, move the new one in. No GUI surface, so once a
real `Update` exists the install is mechanical and hash-assertable.

## Definition of done

- [ ] Two versions built by `cargo tauri build`, published through `delta-release`
- [ ] Local plain-HTTP server, no cloud, no external dependencies
- [ ] Post-install BLAKE3 of the **main binary** matches v_new, by name, by hash
- [ ] Failure branches asserted by outcome, not by error
- [ ] DECISIONS #13 recording the Gatekeeper path taken and what it proves
- [ ] SPRINT / CHANGELOG / docs updated in the same PR

## Tasks

### 1. Example app — done

- [x] `examples/desktop-app`, Tauri v2, plain HTML frontend, no npm build step
- [x] Registers both plugins; real "check for updates" action in the UI
- [x] The update path is the genuine integration: Tauri's own `check()` reads
      the manifest (works because it is a *superset*, decision #5), yielding the
      `Update` that installs; our flow substitutes only the download

### 2. Control surface — done, and verified absent by default

- [x] `#[cfg(feature = "e2e-control")]`, feature **not** in `default`
- [x] Loopback HTTP only: `/trigger`, `/outcome`, `/version`
- [x] Triggers the *same* function the UI button calls, so tested path and
      shipped path are the same code
- [x] **Verified by evidence, not inspection:** default build has no marker
      string in the binary; the check is non-vacuous because the marker *is*
      present when the feature is explicitly enabled

No private key is committed either — the harness generates one per run. A
test-only key in the repo is the same hazard class as a shipped test surface.

### 3. Two published versions — in progress

- [ ] `cargo tauri build` producing `.app` and `.app.tar.gz`
- [ ] v_old and v_new differing in the **main binary**, not just metadata
- [ ] Both published through `delta-release`, not hand-crafted

### 4. Harness

- [ ] Serve manifest + patch + `.app.tar.gz` over the existing `TestServer`
- [ ] Assert v_old != v_new **at the main-binary layer** — different metadata
      around an identical binary is the `appimage_pair` vacuous pass arriving
      through a different door

### 5. The install

- [ ] Direct route: ad-hoc codesign, `xattr -cr`, run `Update::install` on the
      live bundle
- [ ] Assert the installed bundle's main binary hashes to v_new
- [ ] If `PermissionDenied` or an authorization prompt blocks a headless run,
      take the stripped-down fallback and record it — do not paper over it

### 6. Failure branches

- [ ] corrupt / truncated / wrong-base patch → fallback → installed bytes are
      the released artifact
- [ ] signature failure → no install, loud failure, per DECISIONS #11
- [ ] corrupt full download → nothing installed

## In progress

Building the two app versions.

## Blocked

Nothing.

## Notes

- Claim status: **"a running Tauri app accepts a served update" is OPEN.** It
  closes only if the direct install path succeeds. The stripped-down fallback
  closes a weaker claim and leaves the full one open, recorded as such.
- Base-artifact caching is not built; the harness supplies the previous
  artifact directly. A shipping plugin would cache it after each update. That
  gap is real and belongs to Phase 3 proper.

## Next sprint

Phase 4 — robustness: resumable downloads, disk-full, atomic writes, and an
audit of every failure branch. Much of it already exists; the work is closing
gaps rather than starting fresh.
