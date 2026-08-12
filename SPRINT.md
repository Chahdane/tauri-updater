# Sprint 3 — The Tauri plugin

**Phase:** 3 of 8 ([roadmap](docs/ROADMAP.md))
**Started:** 2026-08-12
**Status:** Paused for hardening — see below

---

# POST-CODEX HARDENING AUDIT

**Started:** 2026-08-12
**Status:** Gate A investigation complete, awaiting design approval

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
| 5 | HTTP transport hardening | **CONFIRMED** (7 of 8 sub-claims) | B |
| 6 | Resource exhaustion caps | **CONFIRMED** | B |
| 7 | Atomic file handling | **PARTIALLY CONFIRMED** | B |
| 8 | Concurrent update protection | **CONFIRMED** | B |
| 9 | `delta-release --version` collision | **CONFIRMED** — worse than reported | C |
| 10 | Release workflow assumptions | **CONFIRMED** | C |
| 11 | Tauri compatibility range | **CONFIRMED** | C |
| 12 | Documentation contradictions | **CONFIRMED** (A–E) | D |
| 13 | Plugin DX — design only | Deferred to Phase 5 proposal | — |
| 15 | macOS 95% vs tar 6.6% | Preserve as observation, do not overclaim | D |

## Gate A — trust architecture

- [ ] 1. Update identity: single authoritative response
- [ ] 2. Correct DECISIONS #11 reasoning without changing the fail-closed policy
- [ ] 3. `checked == delta == verified == installed` invariant, structurally testable
- [ ] 4. Downgrade / replay / version policy, with tests

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

`feat/e2e-real-app` (local only, never pushed) carries the example desktop app,
the two-version build script, the E2E harness scripts and the Codex audit
transcript. Not to be reset or deleted. It stacks on `main` at 34a79db.

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
