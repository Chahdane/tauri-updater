//! Minimal Tauri app used to prove a real updater accepts a delta-rebuilt artifact.
//!
//! This exists for one reason: `tauri_plugin_updater::Update` has private
//! fields, so no test can construct one. Only `Updater::check()` inside a
//! running app can produce the handle that `install` needs. A real app is
//! therefore the only way to exercise the real seam — see `docs/DECISIONS.md`.
//!
//! The update it performs is the genuine integration:
//!
//! 1. Tauri's own `Updater::check()` reads the manifest — which works precisely
//!    because our manifest is a *superset* of Tauri's (decision #5), so the
//!    official updater parses it without knowing deltas exist.
//! 2. That yields an `Update`, which is the only thing that can install.
//! 3. Our flow then substitutes the download step: patch, reconstruct, verify.
//! 4. The verified artifact goes to `Update::install` through `TauriInstall`.
//!
//! Step 3 is the whole project. Steps 1, 2 and 4 are stock Tauri.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(feature = "e2e-control")]
mod control;

mod update;

/// The app's own "check for updates" action, invoked from the UI.
///
/// This is what a real app would call. The e2e control surface triggers the
/// same function, so the tested path and the shipped path are the same code.
#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || update::run(&app))
        .await
        .map_err(|e| format!("update task panicked: {e}"))?
}

fn main() {
    let builder = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![check_for_updates])
        // The official updater still owns checking and installing.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Ours makes the download smaller when it can.
        .plugin(
            tauri_plugin_updater_delta::Builder::new()
                .manifest_url(update::manifest_url())
                .build()
                .expect("the delta plugin needs a manifest URL"),
        );

    // Reconciliation runs on every launch, feature or not: it is how the cache
    // learns which version actually came back up, and that is the app's job
    // rather than the harness's. See `update::reconcile_on_launch`.
    let builder = builder.setup(|app| {
        let verdict = update::reconcile_on_launch(app.handle());
        eprintln!("delta: cache reconciliation: {verdict}");

        #[cfg(feature = "e2e-control")]
        control::spawn(app.handle().clone());

        Ok(())
    });

    builder
        .run(tauri::generate_context!())
        .expect("error while running the example app");
}
