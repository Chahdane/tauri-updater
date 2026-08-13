# Sprint 4 — The real-app end-to-end

**Phase:** 3 of 8, final gate ([roadmap](docs/ROADMAP.md))
**Started:** 2026-08-12
**Status:** Paused for hardening — see below

---

# POST-CODEX HARDENING AUDIT

**Started:** 2026-08-12
**Status:** Gates A, B, C and D complete. Real-app E2E remains the open claim.

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

## Gate B — transport and resource safety ✅

- [x] 5. HTTPS (mirroring upstream's flag and profile rule), bounded redirects
      with no HTTPS→HTTP downgrade, a whole-request deadline, and a response
      ceiling enforced both from `Content-Length` and by counting bytes
- [x] 6. `Limits::max_target_bytes` — a ceiling the server does not control
- [x] 7. Reconstruction builds into `.part` and renames after the digest gate;
      downloads do the same
- [x] 8. One `tempfile` workspace per update, replacing the fixed filenames

Chosen mechanisms and the arguments for them are in `docs/DECISIONS.md` #18
(workspace over lock) and #19 (transport policy mirrors Tauri's).

### Mutation evidence

Each guard disabled in turn, intended test confirmed red for the intended
reason, restored. No mutations committed.

| Guard disabled | Killed | Notes |
| --- | --- | --- |
| HTTPS refusal | 1 | `release strictness must refuse http` |
| Redirect budget | 1 | |
| Request deadline | 1 | stall test ran 30 s instead of timing out |
| `Content-Length` pre-check | 1 | |
| Streaming byte counter | 1 | endless body ran to the deadline |
| `.part` promotion | 1 | destroys the cached artifact |
| Local target-size cap | **4** | |
| Per-update workspace | 2 | |

Three of these did not fail on the first attempt, and the tests were what needed
fixing:

- The HTTPS refusal only ran in release builds, where the suite never runs. The
  rule was split into a pure `scheme_verdict(url, insecure, strict)` so the
  release arm is asserted in any profile.
- The redirect test used a self-loop, which *hangs* rather than fails when the
  budget is removed — and a test that hangs under mutation is not evidence. It
  is now a finite chain longer than the budget.
- The partial-file test asserted only "nothing left behind", which is equally
  true of writing to the destination and deleting it on error. It now asserts
  the property `.part` actually buys: a failed download must not destroy the
  artifact already at that path.

## Gate C — concrete defects ✅

- [x] 9. `--version` → `--target-version`, with `tests/cli.rs` running the real
      executable in debug. Six tests, all of which fail if the collision returns.
- [x] 10. The tag path derives everything it needs — builds the AppImage, finds
      the previous release, downloads its artifact, generates and checks the
      manifest before upload. Remaining manual prerequisites are listed in the
      workflow header. The rehearsal now runs the CLI tests and the
      release-to-client loop instead of `--help`.
- [x] 11. `tauri-plugin-updater` narrowed to `>=2.10.1, <2.11.0`, enforced by
      `tests/upstream_compat.rs` reading the resolved version out of `Cargo.lock`.

### Mutation evidence

| Guard disabled | Killed |
| --- | --- |
| CLI argument collision reintroduced | **6** |
| Version requirement widened to `"2"` | 1 |
| Verified-version list made stale | 1 |

## Gate D — truthfulness ✅

- [x] 12. Documentation audit. Seven contradictions found by re-reading each
      document against the source, plus one real defect (`Builder::manifest_url`
      is required but never read since #13 removed the fetch that consumed it).
- [x] 16. `research/` with a 33-field schema, a findings ledger, and empty
      `experiments/` and `logs/` directories. No historical data was
      reconstructed: the two macOS ratios are recorded as STRONG OBSERVATION
      with their missing provenance stated, and the compression explanation for
      them stays a HYPOTHESIS.
- [x] 25. Conservative claim language. README now carries a supported /
      partially supported / unproven table, with a real running Tauri app
      installing a delta update listed as **unproven**.

### Research ledger, by classification

| Class | Count | Notes |
| --- | --- | --- |
| DEMONSTRATED | 8 | Reproducible in this repository |
| STRONG OBSERVATION | 4 | Includes both macOS ratios, provenance missing |
| HYPOTHESIS | 2 | Compression-representation explanation; build-noise floor |
| ENGINEERING DECISION | 2 | |
| UNPROVEN | 3 | Includes the real-app install |

## Preserved work — now in `main`

`feat/e2e-real-app` carried the example desktop app, the two-version build
script, the E2E harness scripts and the Codex audit transcript. It was **merged
into `main` as PR #12 out of order**, before Gate A; see `docs/DECISIONS.md` #17.
Not reverted.

Three consequences, all handled in the Gate A PR:

- **The example was ported** to `UpdateExt::delta_identity`. It is a workspace
  member, so Gate A could not compile until it moved.
- **`main` was red on all five CI jobs** when #12 landed. A stale `Cargo.lock`
  (example `Cargo.toml` at 1.0.0, lock recording 1.0.1) failed every `--locked`
  job — four of the five — and `crates/delta-release/src/signing.rs` failed
  `cargo fmt --check`. Both fixed here.
- **A GitHub "Update branch" merge on the PR dropped Gate A's DECISIONS #13 and
  #14**, leaving four code comments pointing at the wrong sections. Resolved by
  keeping both sets and renumbering: Gate A holds #13/#14, the macOS and rsign2
  entries moved to #15/#16.

### First tasks when real-app E2E resumes

1. **Review `examples/desktop-app/e2e/*.sh`.** Deliberately untouched. They were
   written against the two-fetch flow, and `DELTA_E2E_MANIFEST_URL` now feeds
   only Tauri's `endpoints()` — the harness may still assume the plugin fetches
   the manifest itself.
2. **Validate the port at runtime.** It compiles and reads well, but no running
   app has exercised `delta_identity()` yet. That remains the open claim.
3. **Stop `build-versions.sh` re-introducing the lock drift** that broke `main`;
   it rewrites the example's version in place.

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
