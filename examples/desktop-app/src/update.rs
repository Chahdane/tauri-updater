//! Normal application integration: one official check, then one verified install.
//!
//! Cache paths, launch reconciliation, public-key lookup, transport policy,
//! reconstruction, and the install handoff are plugin-managed. The frontend can
//! invoke the command in `main.rs`; it does not need a delta-specific SDK.

use std::sync::Mutex;

use tauri::{AppHandle, Runtime};
use tauri_plugin_updater_delta::{DeltaUpdaterExt, Outcome, ProgressEvent};

/// The latest progress line, polled by the page through `update_progress`.
///
/// A plain app command rather than a Tauri event, so the example needs no
/// extra capability to show it.
static PROGRESS: Mutex<String> = Mutex::new(String::new());

/// The most recent progress line, for the page to display.
pub fn progress() -> String {
    PROGRESS.lock().map(|line| line.clone()).unwrap_or_default()
}

fn describe(event: ProgressEvent) -> String {
    match event {
        ProgressEvent::Checking => "checking".to_owned(),
        ProgressEvent::Downloading => "downloading".to_owned(),
        ProgressEvent::DownloadProgress {
            downloaded,
            total: Some(total),
        } => format!("downloading {downloaded} of {total} bytes"),
        ProgressEvent::DownloadProgress { downloaded, .. } => {
            format!("downloading {downloaded} bytes")
        }
        ProgressEvent::Reconstructing => "reconstructing".to_owned(),
        ProgressEvent::Verifying => "verifying".to_owned(),
        ProgressEvent::Installing => "installing".to_owned(),
        ProgressEvent::Finished => "finished".to_owned(),
        _ => "working".to_owned(),
    }
}

/// Check for and install one update, returning a concise UI-friendly result.
pub async fn run<R: Runtime>(app: &AppHandle<R>) -> Result<String, String> {
    let Some(update) = app
        .delta_updater()
        .check_with(|event| {
            if let Ok(mut line) = PROGRESS.lock() {
                *line = describe(event);
            }
        })
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

    // The E2E harness matches on the leading label, so it stays first.
    let label = match outcome {
        Outcome::InstalledFromTarDelta { .. } => "installed-from-tar-delta",
        Outcome::InstalledFromDirectDelta { .. } => "installed-from-delta",
        Outcome::InstalledFromFullDownload { .. } => "installed-from-full-download",
        Outcome::UpToDate { .. } => return Ok("up-to-date".to_owned()),
        _ => "update-completed",
    };
    Ok(
        match (
            outcome.downloaded_bytes(),
            outcome.full_artifact_size(),
            outcome.bytes_saved(),
        ) {
            (Some(downloaded), Some(full), Some(saved)) => {
                format!("{label} downloaded={downloaded} full={full} saved={saved}")
            }
            _ => label.to_owned(),
        },
    )
}
