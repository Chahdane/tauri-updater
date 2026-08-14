//! Tests the application-facing seam instead of assembling the internal flow.

// The supported v0.1 application path is macOS. Under a unified Windows
// workspace build, Tauri's mock-test feature produces a test executable that
// the runner cannot load (STATUS_ENTRYPOINT_NOT_FOUND) before any assertion
// runs. Keep the API proof on macOS and Linux; Windows continues to run every
// engine, transport, release, and compile test without claiming an app path.
#![cfg(not(target_os = "windows"))]

#[allow(dead_code)]
mod support;

use std::time::Duration;

use support::server::TestServer;
use tauri_plugin_updater_delta::DeltaUpdaterExt;

#[test]
fn one_public_check_makes_one_authoritative_manifest_request() {
    let server = TestServer::start();
    let platform = tauri_updater_delta_core::release_identity::current_platform();
    let manifest = serde_json::json!({
        "version": "1.0.1",
        "platforms": {
            platform: {
                "url": server.url("/artifact.app.tar.gz"),
                "signature": "selected-by-the-official-updater"
            }
        }
    });
    server.serve(
        "/manifest.json",
        serde_json::to_vec(&manifest).expect("serialize manifest"),
    );

    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = "dev.example.delta-api-test".to_owned();
    context.config_mut().plugins.0.insert(
        "updater".to_owned(),
        serde_json::json!({
            "endpoints": ["https://releases.example.invalid/manifest.json"],
            "pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEEwODlCMTQ3RTk5OTdGMjEKUldRaGY1bnBSN0dKb01wcFhKWHRhRnRWM1ZCUHB3ZTBBbEJ3SFo4WmZVQzVlQWVyVGx5WnlBTXQK"
        }),
    );

    let cache = tempfile::tempdir().expect("cache tempdir");
    let app = tauri::test::mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_updater_delta::Builder::new()
                .cache_dir(cache.path())
                .endpoint_for_tests(server.url("/manifest.json"))
                .build(),
        )
        .build(context)
        .expect("build mock app");

    let update = tauri::async_runtime::block_on(app.delta_updater().check())
        .expect("check succeeds")
        .expect("newer release");
    assert_eq!(update.current_version(), "0.1.0");
    assert_eq!(update.version(), "1.0.1");

    // A second plugin-owned manifest lookup would be observable even if it ran
    // just after the authoritative check returned.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(server.request_count(), 1);
}

#[test]
fn normal_example_is_https_only_and_does_not_enable_the_harness() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("examples/desktop-app/tauri.conf.json"))
            .expect("read example config"),
    )
    .expect("parse example config");
    let endpoints = config["plugins"]["updater"]["endpoints"]
        .as_array()
        .expect("updater endpoints");
    assert!(
        endpoints.iter().all(|endpoint| endpoint
            .as_str()
            .is_some_and(|url| url.starts_with("https://"))),
        "the normal example must not configure plain HTTP"
    );
    assert!(
        config["plugins"]["updater"]
            .get("dangerousInsecureTransportProtocol")
            .is_none(),
        "the normal example must not relax Tauri's transport policy"
    );

    let manifest = std::fs::read_to_string(root.join("examples/desktop-app/Cargo.toml"))
        .expect("read example manifest");
    assert!(manifest.contains("default = []"));
    assert!(!manifest.contains("default = [\"e2e-control\"]"));
}
