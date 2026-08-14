//! Normal application integration: one official check, then one verified install.
//!
//! Cache paths, launch reconciliation, public-key lookup, transport policy,
//! reconstruction, and the install handoff are plugin-managed. The frontend can
//! invoke the command in `main.rs`; it does not need a delta-specific SDK.

use tauri::{AppHandle, Runtime};
use tauri_plugin_updater_delta::{DeltaUpdaterExt, Outcome};

/// Check for and install one update, returning a concise UI-friendly result.
pub async fn run<R: Runtime>(app: &AppHandle<R>) -> Result<String, String> {
    let Some(update) = app
        .delta_updater()
        .check()
        .await
        .map_err(|error| format!("update check failed: {error}"))?
    else {
        return Ok("up-to-date".to_owned());
    };

    let outcome = update
        .install()
        .await
        .map_err(|error| format!("update failed: {error}"))?;

    for diagnostic in outcome.diagnostics() {
        // Non-fatal: the current install succeeded, but a future update may
        // need another Full download until the cache becomes writable.
        log::warn!("{diagnostic}");
    }

    Ok(match outcome {
        Outcome::InstalledFromTarDelta {
            downloaded,
            full_download_size,
            ..
        } => {
            format!("installed-from-tar-delta downloaded={downloaded} full={full_download_size}")
        }
        Outcome::InstalledFromDirectDelta {
            downloaded,
            full_download_size,
            ..
        } => {
            format!("installed-from-delta downloaded={downloaded} full={full_download_size}")
        }
        Outcome::InstalledFromFullDownload { .. } => "installed-from-full-download".to_owned(),
        Outcome::UpToDate { .. } => "up-to-date".to_owned(),
        _ => "update-completed".to_owned(),
    })
}
