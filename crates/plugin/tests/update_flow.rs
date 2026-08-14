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

/// The platform this test runs on, not a hard-coded one.
///
/// The release signature now authenticates the platform it was built for, so a
/// fixture claiming `linux-x86_64` is correctly refused on a macOS runner. That
/// is the guard working, and the fixture is what has to describe the machine.
fn platform() -> String {
    tauri_updater_delta_core::release_identity::current_platform()
}

use minisign::KeyPair;
use tauri_plugin_updater_delta::flow::{run_update, Context, InstallHandoff, Outcome};
use tauri_plugin_updater_delta::Error;
use tauri_updater_delta_core::client::Fetch;
use tauri_updater_delta_core::{Limits, Refusal, UpdateIdentity, VerifiedArtifact};
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{build_release, ReleaseRequest};

const MANIFEST_URL: &str = "https://example.com/manifest.json";
const PATCH_URL: &str = "https://example.com/patch.zst";
const INSTALLER_URL: &str = "https://example.com/app_1.0.1.AppImage";

struct FakeServer {
    files: RefCell<HashMap<String, Vec<u8>>>,
    /// Every URL requested, in order. Lets a test assert on what was *not*
    /// fetched, which is the only way to prove the second manifest request is
    /// gone rather than merely moved.
    requested: RefCell<Vec<String>>,
}

impl Fetch for FakeServer {
    fn fetch(&self, url: &str, out: &Path) -> Result<(), String> {
        self.requested.borrow_mut().push(url.to_owned());
        let map = self.files.borrow();
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
    /// The document the release tooling produced, exactly as a server would
    /// serve it and as Tauri would retain it on `Update::raw_json`.
    manifest_json: String,
    /// The signature Tauri's platform search would have selected from it.
    signature: String,
}

impl World {
    /// The identity an honest `Updater::check()` produces for this release.
    fn identity(&self, current_version: &str) -> UpdateIdentity {
        UpdateIdentity::new(
            current_version,
            "1.0.1",
            platform(),
            INSTALLER_URL,
            &self.signature,
            &self.manifest_json,
        )
    }
}

/// Run the real release tooling and stand up a server holding what it produced.
fn world(dir: &Path, pair: &KeyPair) -> World {
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir);
    let patch = dir.join("patch.zst");

    let key = SigningKey::from_str(&pair.sk.to_box(None).expect("box key").into_string(), None)
        .expect("load key");

    let (manifest, _) = build_release(
        &ReleaseRequest {
            platform: &platform(),
            version: "1.0.1",
            from_version: "1.0.0",
            previous_installer: &fixture.old,
            new_installer: &fixture.new,
            installer_url: INSTALLER_URL,
            patch_url: PATCH_URL,
            patch_out: &patch,
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            tar_layer: None,
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
        server: FakeServer {
            files: RefCell::new(files),
            requested: RefCell::new(Vec::new()),
        },
        pubkey: pubkey_b64(pair),
        base: fixture.old,
        released: fixture.new,
        signature: manifest.platforms[&platform()].signature.clone(),
        manifest_json: manifest.to_json().expect("serialise"),
    }
}

fn run(
    w: &World,
    handoff: &RecordingHandoff,
    work: &Path,
    base: Option<&Path>,
) -> tauri_plugin_updater_delta::Result<Outcome> {
    run_with(w, &w.identity("1.0.0"), handoff, work, base)
}

fn run_with(
    w: &World,
    identity: &UpdateIdentity,
    handoff: &RecordingHandoff,
    work: &Path,
    base: Option<&Path>,
) -> tauri_plugin_updater_delta::Result<Outcome> {
    run_update(
        identity,
        &Context {
            pubkey: &w.pubkey,
            base,
            cache: None,
            app_id: "dev.example.testapp",
            work_dir: work,
            limits: Limits::default(),
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
        .files
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
    w.server.files.borrow_mut().remove(PATCH_URL);
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
        .files
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
        .files
        .borrow_mut()
        .insert(PATCH_URL.to_owned(), b"junk".to_vec());
    w.server
        .files
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

// ---- Gate A: the update identity, against real signatures ---------------
//
// These are the cases where verification genuinely succeeds and is genuinely
// not enough. Everything above proves unverified bytes cannot be installed;
// these prove the *right* release is installed, which is a different claim.

#[test]
fn replaying_an_old_genuinely_signed_release_installs_nothing() {
    // Gate A test 5, and the attack the whole gate exists for.
    //
    // The attacker needs no signing key. They take a real past release —
    // manifest, artifact and signature all authentic, and the signature will
    // verify forever because minisign signatures do not expire — and serve it to
    // a client already running something newer. Every cryptographic check
    // passes. Only the version policy can refuse it.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let handoff = RecordingHandoff::default();

    // Same release, same real signature, same real artifact — replayed at a
    // client on 2.0.0.
    let replayed = w.identity("2.0.0");

    let result = run_with(
        &w,
        &replayed,
        &handoff,
        &dir.path().join("work"),
        Some(&w.base),
    );

    assert!(
        matches!(result, Err(Error::Refused(Refusal::Downgrade { .. }))),
        "a replayed older release must be refused, got {result:?}"
    );
    assert!(
        handoff.installed.borrow().is_empty(),
        "the artifact verifies correctly — only the version policy stops it"
    );
}

#[test]
fn the_replayed_artifact_really_does_verify() {
    // The test above is only meaningful if the bytes it refuses would otherwise
    // have been installed. This proves the refusal is doing the work, rather
    // than the signature check quietly failing for an unrelated reason.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let handoff = RecordingHandoff::default();

    // The identical release, offered to a client on an older version.
    let outcome = run_with(
        &w,
        &w.identity("1.0.0"),
        &handoff,
        &dir.path().join("work"),
        Some(&w.base),
    )
    .expect("the same artifact installs cleanly when it is genuinely an upgrade");

    assert!(matches!(outcome, Outcome::InstalledFromDelta { .. }));
    assert_eq!(handoff.installed.borrow().len(), 1);
}

#[test]
fn a_version_tauri_never_checked_installs_nothing() {
    // Gate A test 1 against a real signed release: the document describes
    // 1.0.1, but Tauri was told 9.9.9 so its semver gate would pass. Under the
    // old two-fetch architecture these were separate responses and nothing
    // compared them.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let handoff = RecordingHandoff::default();

    let lying = UpdateIdentity::new(
        "1.0.0",
        "9.9.9",
        platform(),
        INSTALLER_URL,
        &w.signature,
        &w.manifest_json,
    );

    let result = run_with(
        &w,
        &lying,
        &handoff,
        &dir.path().join("work"),
        Some(&w.base),
    );

    assert!(
        matches!(
            result,
            Err(Error::Refused(Refusal::IdentityMismatch {
                field: "version",
                ..
            }))
        ),
        "expected a version identity mismatch, got {result:?}"
    );
    assert!(handoff.installed.borrow().is_empty());
}

#[test]
fn the_manifest_is_never_fetched() {
    // Gate A test 10. The flow has no manifest URL any more, so the only
    // request a delta install may make is the patch. Asserting on the server's
    // request log rather than on the type signature keeps this honest if a
    // fetch is ever reintroduced somewhere less visible.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let handoff = RecordingHandoff::default();

    run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");

    let requested = w.server.requested.borrow();
    assert!(
        !requested.iter().any(|url| url == MANIFEST_URL),
        "the manifest must never be fetched a second time: {requested:?}"
    );
    assert_eq!(
        requested.as_slice(),
        &[PATCH_URL.to_owned()],
        "a delta install downloads the patch and nothing else"
    );
}
