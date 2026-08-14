//! Minimal, normal Rust-first integration for the delta updater.
//!
//! The `e2e-control` feature adds the repository's localhost harness around this
//! same path. It is absent from default builds.

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
    update::run(&app).await
}

fn main() {
    let delta_builder = tauri_plugin_updater_delta::Builder::new();
    #[cfg(feature = "e2e-control")]
    let delta_builder = control::configure_delta(delta_builder);

    let builder = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![check_for_updates])
        // The official updater still owns checking and installing.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Ours makes the download smaller when it can.
        // No configuration: the flow reads the release document out of the
        // response Tauri's own check already made.
        .plugin(delta_builder.build());

    let builder = builder.setup(|app| {
        #[cfg(feature = "e2e-control")]
        control::spawn(app.handle().clone());

        Ok(())
    });

    builder
        .run(tauri::generate_context!())
        .expect("error while running the example app");
}
