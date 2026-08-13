//! The real update path: Tauri checks, we substitute the download, Tauri installs.
//!
//! Not feature-gated — this is what the app does, and it is identical whether or
//! not the test control surface is compiled in. The control surface only
//! *triggers* it.
//!
//! # What this file is evidence of
//!
//! It is the smallest honest answer to "what does adopting this plugin cost a
//! Tauri developer?". Everything below the `updater.check()` call is the plugin;
//! everything above it is stock `tauri-plugin-updater` that the app would have
//! written anyway. See `docs/DECISIONS.md` #13 for why the update must originate
//! from Tauri's own check rather than a second fetch of our own.

use std::path::PathBuf;

use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_updater_delta::flow::{run_update, Context, Outcome};
use tauri_plugin_updater_delta::{HttpFetch, Limits, TauriInstall, UpdateExt};

/// Where the manifest lives.
///
/// Read from the environment so the harness can serve on an ephemeral port;
/// a real app would hard-code this or take it from its own config.
///
/// Note this now feeds **only** Tauri's `endpoints()`. The plugin never fetches
/// it — that is the whole point of #13 — so there is exactly one place in this
/// app that knows a manifest URL exists.
pub fn manifest_url() -> String {
    std::env::var("DELTA_E2E_MANIFEST_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:0/manifest.json".to_owned())
}

/// The previously installed artifact, if one was kept.
///
/// A shipping plugin caches this after each update — that is Phase 3 work not
/// yet built. The harness supplies it directly so the delta path can be
/// exercised now, and the distinction is recorded in DECISIONS.
fn base_artifact() -> Option<PathBuf> {
    std::env::var("DELTA_E2E_BASE_ARTIFACT")
        .ok()
        .map(PathBuf::from)
}

/// Run one update, returning a short description of what happened.
pub fn run<R: Runtime>(app: &AppHandle<R>) -> Result<String, String> {
    // The public key comes from tauri.conf.json — the same value the official
    // updater would verify against. Not passed in by the harness, because a
    // harness-supplied key would prove less.
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str())
        .ok_or("no updater pubkey in tauri.conf.json")?
        .to_owned();

    // Tauri's own check. This is the step that proves the superset manifest
    // works: the official updater parses it with no knowledge of deltas, and
    // keeps the whole document on `Update::raw_json` for us to read.
    let updater = app
        .updater_builder()
        .endpoints(vec![manifest_url()
            .parse()
            .map_err(|e| format!("bad manifest url: {e}"))?])
        .map_err(|e| format!("configuring the updater: {e}"))?
        .build()
        .map_err(|e| format!("building the updater: {e}"))?;

    let update = tauri::async_runtime::block_on(updater.check())
        .map_err(|e| format!("update check failed: {e}"))?;

    let Some(update) = update else {
        return Ok("up-to-date".to_owned());
    };

    // Out of the async context now, so the blocking flow is safe to run.
    let work_dir = std::env::temp_dir().join("delta-updater-example-work");
    let fetch = HttpFetch::new().map_err(|e| format!("http client: {e}"))?;
    let base = base_artifact();

    let outcome = run_update(
        // One line, and it is the entire trust argument: the release we install
        // is the release Tauri checked, because it is the same object.
        &update.delta_identity(),
        &Context {
            pubkey: &pubkey,
            base: base.as_deref(),
            work_dir: &work_dir,
            limits: Limits::default(),
        },
        &fetch,
        // The real seam. Everything before this produced a VerifiedArtifact;
        // this hands it to the official updater's own install step.
        &TauriInstall::new(&update),
    )
    .map_err(|e| format!("update failed: {e}"))?;

    Ok(match outcome {
        Outcome::InstalledFromDelta {
            downloaded,
            saved_against,
        } => format!("installed-from-delta downloaded={downloaded} full={saved_against}"),
        Outcome::InstalledFromFullDownload => "installed-from-full-download".to_owned(),
        Outcome::UpToDate => "up-to-date".to_owned(),
    })
}
