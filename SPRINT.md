# Sprint 4 — The real-app end-to-end

**Phase:** 3 of 8, final gate ([roadmap](docs/ROADMAP.md))
**Started:** 2026-08-12
**Status:** In progress

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
