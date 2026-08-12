# Sprint 3 — The Tauri plugin

**Phase:** 3 of 8 ([roadmap](docs/ROADMAP.md))
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

First end-to-end delta update in a real Tauri app, on Linux/AppImage: the plugin
wraps `tauri-plugin-updater`, fetches the manifest, picks a patch, applies it,
verifies it, and hands the finished artifact to Tauri's own install step.

## Definition of done

- [ ] Green matrix — fmt + clippy + tests on Linux, macOS, Windows
- [ ] An example app updates itself from a patch
- [ ] Every failure path demonstrably falls back to a full download
- [ ] SPRINT / CHANGELOG / docs updated in the same PR

## Tasks

### Signature compatibility — done, and done first

This gated everything else: if Tauri rejected our signatures, the manifest format
and the whole handoff design would have needed rework, and finding that out after
building a client on top would have been expensive.

- [x] Reproduce `tauri-plugin-updater` 2.10.1's `verify_signature` verbatim and
      run it against real `delta-release` output
- [x] Confirm the signature is **prehashed**, proven two independent ways rather
      than inferred from a green test
- [x] Confirm Tauri rejects wrong bytes and wrong keys
- [x] Confirm a **delta-rebuilt** artifact satisfies the same signature
- [x] Record the result and the remaining gap in `docs/DECISIONS.md` #6

Result: cleared at the algorithm level. Six tests. The remaining gap — a
*running* Tauri app accepting an update served this way — is the example app
below, and is deliberately tracked as a separate claim.

### Verification is the sole gate — enforced by the type system

Tauri does not verify what it is handed (DECISIONS #10), so this plugin's check
is the only one. A sole gate with no backstop must not depend on call sites
remembering it.

- [x] `VerifiedArtifact` — private field, no public constructor, so
      `verify_artifact` is the only way in the language to obtain one
- [x] Token owns the verified *bytes*, not a path, closing the window where a
      file could be swapped between check and handoff
- [x] `compile_fail` doctests proving forgery does not build, mutation-tested so
      they are not vacuously green
- [x] Handoff signature takes `&VerifiedArtifact`, so unverified bytes cannot be
      expressed at the call site

### Plugin crate — client flow done, Tauri glue next

- [x] `tauri-plugin-updater-delta` crate with the full client flow
- [x] Manifest fetch from a configured URL
- [x] Patch selection for the installed version and current platform
- [x] Patch download, with the manifest's digest checked before applying
- [x] Reconstruct via `try_reconstruct`, then verify, then hand off
- [x] Full-download fallback wired into every failure path
- [x] `HttpFetch` — blocking HTTP over reqwest, the stack `tauri-plugin-updater`
      itself uses
- [x] `InstallHandoff` implementation over `tauri_plugin_updater::Update`
- [x] Plugin registration (`tauri::plugin::Builder`) and configuration
- [x] Linux CI: webkit2gtk apt dependencies, now `tauri` is in the build

### Base artifact

- [ ] Cache the installer after a successful update, so the next update has a base
- [ ] Behave correctly when no base is available — plain full download, no error

### Install handoff

- [ ] AppImage adapter: hand the verified artifact to Tauri's install step
- [ ] Keep the seam narrow enough that Windows and macOS adapters slot in later

### Example app

- [ ] Minimal Tauri app using both plugins
- [ ] Serve a manifest and artifacts locally
- [ ] **Prove a real Tauri updater accepts a delta-rebuilt artifact end to end** —
      closes the remaining gap in DECISIONS #6
- [ ] Real-world patch ratio numbers for the README

### Docs

- [ ] README install and configuration, replacing the aspirational example
- [ ] ARCHITECTURE: the client flow as built
- [ ] DECISIONS: base-artifact caching strategy

## In progress

The example app — a real Tauri updater accepting a served update. This is the
only remaining claim, and nothing green so far substitutes for it: every test to
date uses a fake handoff, by design.

## Blocked

Nothing.

## Notes

- `gh` is still not installed, so PRs are opened by hand. Reading CI *status*
  works anonymously now the repo is public; reading CI *logs* needs
  `gh auth login`.
- Two PRs are open and awaiting review: `fix/msrv`, and `feat/release-tooling`
  stacked on it. This sprint's branch stacks on those in turn.
- Phase 2 exit state: 63 tests green across Linux, macOS and Windows.

## Next sprint

Phase 4 — robustness and security: forcing every failure path in tests, resume
and retry for interrupted downloads, disk-space checks, and fuzzing the apply
path against malformed patches.
