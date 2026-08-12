//! The client flow against a manifest the real release tool produced.
//!
//! Nothing here is hand-built: the release path runs, and the client is given
//! only the manifest JSON and a fake server holding the files it names. Both
//! boundaries — network and installer — are fakes, which is what lets the whole
//! path run offline with no Tauri runtime.
//!
//! What the assertions are really about is the safety invariant: **whatever goes
//! wrong, the bytes that reach the installer are either correct or absent.**
//! There is no case in which something unverified is installed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use base64::Engine as _;
use minisign::KeyPair;
use tauri_plugin_updater_delta::flow::{run_update, Context, InstallHandoff, Outcome};
use tauri_plugin_updater_delta::Error;
use tauri_updater_delta_core::client::Fetch;
use tauri_updater_delta_core::VerifiedArtifact;
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{build_release, ReleaseRequest};

const PLATFORM: &str = "linux-x86_64";
const MANIFEST_URL: &str = "https://example.com/manifest.json";
const PATCH_URL: &str = "https://example.com/patch.zst";
const INSTALLER_URL: &str = "https://example.com/app_1.0.1.AppImage";

struct FakeServer(RefCell<HashMap<String, Vec<u8>>>);

impl Fetch for FakeServer {
    fn fetch(&self, url: &str, out: &Path) -> Result<(), String> {
        let map = self.0.borrow();
        let body = map.get(url).ok_or_else(|| format!("404 {url}"))?;
        std::fs::write(out, body).map_err(|e| e.to_string())
    }
}

#[derive(Default)]
struct RecordingHandoff {
    installed: RefCell<Vec<Vec<u8>>>,
}

impl InstallHandoff for RecordingHandoff {
    fn install(&self, artifact: &VerifiedArtifact) -> tauri_plugin_updater_delta::Result<()> {
        self.installed
            .borrow_mut()
            .push(artifact.as_bytes().to_vec());
        Ok(())
    }
}

fn keypair() -> KeyPair {
    KeyPair::generate_encrypted_keypair(Some(String::new())).expect("generate keypair")
}

fn pubkey_b64(pair: &KeyPair) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(pair.pk.to_box().expect("box pk").into_string())
}

struct World {
    server: FakeServer,
    pubkey: String,
    base: std::path::PathBuf,
    released: std::path::PathBuf,
}

/// Run the real release tooling and stand up a server holding what it produced.
fn world(dir: &Path, pair: &KeyPair) -> World {
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir);
    let patch = dir.join("patch.zst");

    let key = SigningKey::from_str(&pair.sk.to_box(None).expect("box key").into_string(), None)
        .expect("load key");

    let (manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            from_version: "1.0.0",
            previous_installer: &fixture.old,
            new_installer: &fixture.new,
            installer_url: INSTALLER_URL,
            patch_url: PATCH_URL,
            patch_out: &patch,
            notes: None,
            pub_date: None,
        },
        &key,
        None,
    )
    .expect("release should build");

    let files = HashMap::from([
        (
            MANIFEST_URL.to_owned(),
            manifest.to_json().expect("serialise").into_bytes(),
        ),
        (
            PATCH_URL.to_owned(),
            std::fs::read(&patch).expect("read patch"),
        ),
        (
            INSTALLER_URL.to_owned(),
            std::fs::read(&fixture.new).expect("read installer"),
        ),
    ]);

    World {
        server: FakeServer(RefCell::new(files)),
        pubkey: pubkey_b64(pair),
        base: fixture.old,
        released: fixture.new,
    }
}

fn run(
    w: &World,
    handoff: &RecordingHandoff,
    work: &Path,
    base: Option<&Path>,
) -> tauri_plugin_updater_delta::Result<Outcome> {
    run_update(
        MANIFEST_URL,
        &Context {
            platform: PLATFORM,
            current_version: "1.0.0",
            pubkey: &w.pubkey,
            base,
            work_dir: work,
        },
        &w.server,
        handoff,
    )
}

#[test]
fn installs_from_a_patch_when_it_can() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let handoff = RecordingHandoff::default();

    let outcome =
        run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");

    let Outcome::InstalledFromDelta {
        downloaded,
        saved_against,
    } = outcome
    else {
        panic!("expected a delta install, got {outcome:?}");
    };
    assert!(downloaded < saved_against);

    // The bytes that reached the installer are the released artifact exactly.
    let installed = handoff.installed.borrow();
    assert_eq!(installed.len(), 1);
    assert_eq!(
        installed[0],
        std::fs::read(&w.released).expect("read released")
    );
}

#[test]
fn installs_from_a_full_download_when_there_is_no_base() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let handoff = RecordingHandoff::default();

    let outcome = run(&w, &handoff, &dir.path().join("work"), None).expect("should update");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    let installed = handoff.installed.borrow();
    assert_eq!(
        installed[0],
        std::fs::read(&w.released).expect("read released")
    );
}

#[test]
fn a_corrupt_patch_still_installs_the_right_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    w.server
        .0
        .borrow_mut()
        .insert(PATCH_URL.to_owned(), b"not a patch".to_vec());
    let handoff = RecordingHandoff::default();

    // The delta path fails, the full download takes over, and the user still
    // ends up with the correct artifact. This is the whole safety model in one
    // assertion.
    let outcome =
        run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    let installed = handoff.installed.borrow();
    assert_eq!(
        installed[0],
        std::fs::read(&w.released).expect("read released")
    );
}

#[test]
fn a_patch_server_outage_still_installs_the_right_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    w.server.0.borrow_mut().remove(PATCH_URL);
    let handoff = RecordingHandoff::default();

    let outcome =
        run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");
    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert_eq!(
        handoff.installed.borrow()[0],
        std::fs::read(&w.released).expect("read released")
    );
}

#[test]
fn a_wrong_public_key_installs_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    w.pubkey = pubkey_b64(&keypair()); // app configured with a different key
    let handoff = RecordingHandoff::default();

    let result = run(&w, &handoff, &dir.path().join("work"), Some(&w.base));

    assert!(matches!(result, Err(Error::Signature(_))));
    assert!(
        handoff.installed.borrow().is_empty(),
        "a signature failure must never reach the installer"
    );
}

#[test]
fn a_tampered_full_download_installs_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    w.server
        .0
        .borrow_mut()
        .insert(INSTALLER_URL.to_owned(), b"a malicious installer".to_vec());
    let handoff = RecordingHandoff::default();

    // No base, so this goes straight down the full-download path — the one the
    // official updater would have taken, with the check we perform ourselves.
    let result = run(&w, &handoff, &dir.path().join("work"), None);

    assert!(matches!(result, Err(Error::Signature(_))));
    assert!(handoff.installed.borrow().is_empty());
}

#[test]
fn a_tampered_artifact_served_as_a_patch_target_installs_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    // Break both: the patch is junk and the full artifact is malicious.
    w.server
        .0
        .borrow_mut()
        .insert(PATCH_URL.to_owned(), b"junk".to_vec());
    w.server
        .0
        .borrow_mut()
        .insert(INSTALLER_URL.to_owned(), b"a malicious installer".to_vec());
    let handoff = RecordingHandoff::default();

    let result = run(&w, &handoff, &dir.path().join("work"), Some(&w.base));

    assert!(matches!(result, Err(Error::Signature(_))));
    assert!(
        handoff.installed.borrow().is_empty(),
        "falling back must not lower the bar"
    );
}

#[test]
fn nothing_is_left_in_the_work_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let work = dir.path().join("work");
    let handoff = RecordingHandoff::default();

    run(&w, &handoff, &work, Some(&w.base)).expect("should update");

    for leftover in ["update.patch", "update.artifact", "full.artifact"] {
        assert!(
            !work.join(leftover).exists(),
            "{leftover} should not survive a completed update"
        );
    }
}
