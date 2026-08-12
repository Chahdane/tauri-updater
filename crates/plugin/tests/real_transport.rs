//! The update flow over real HTTP, against artifacts the real release tool made.
//!
//! Every other suite drives the flow through a fake `Fetch`. That proves the
//! logic and proves nothing about [`HttpFetch`] — the code that actually opens a
//! socket, checks a status line and streams a body to disk. This closes that
//! gap: a real server on a real port, real requests, real responses.
//!
//! Two boundaries remain fakes here, deliberately:
//!
//! - The **installer**, which is still a recording [`InstallHandoff`]. Replacing
//!   it with `TauriInstall` needs a running app, which is the example-app work.
//! - Nothing else. The manifest, the patch and the artifact are all produced by
//!   `delta-release` and served over the wire.
//!
//! Assertions are strong-form throughout: not "no error was returned" but *what
//! bytes reached the installer*, compared by BLAKE3 against the released
//! artifact.

mod support;

use std::cell::RefCell;
use std::path::Path;

use base64::Engine as _;
use minisign::KeyPair;
use support::server::{read, Route, TestServer};
use tauri_plugin_updater_delta::flow::{run_update, Context, InstallHandoff, Outcome};
use tauri_plugin_updater_delta::{Error, HttpFetch};
use tauri_updater_delta_core::{FileHash, UpdateIdentity, VerifiedArtifact};
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{build_release, ReleaseRequest};

const PLATFORM: &str = "linux-x86_64";

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

impl RecordingHandoff {
    /// Strong form: exactly one install, and its bytes hash to `expected`.
    fn assert_installed_exactly(&self, expected: &FileHash) {
        let installed = self.installed.borrow();
        assert_eq!(installed.len(), 1, "expected exactly one install");
        assert_eq!(
            FileHash::of_bytes(&installed[0]),
            *expected,
            "the installer received the wrong bytes"
        );
    }

    fn assert_nothing_installed(&self) {
        assert!(
            self.installed.borrow().is_empty(),
            "nothing should have reached the installer"
        );
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
    server: TestServer,
    pubkey: String,
    base: std::path::PathBuf,
    released_hash: FileHash,
    /// The document Tauri would have retained on `Update::raw_json`.
    manifest_json: String,
    /// The signature Tauri's platform search would have selected from it.
    signature: String,
}

/// Publish two versions with the real release tool and serve the result.
fn world(dir: &Path, pair: &KeyPair) -> World {
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir);

    // The fixture already refuses to produce identical versions, so a vacuous
    // pass is structurally impossible. Restate it here so this suite does not
    // silently depend on a guarantee living in another crate.
    let old_hash = FileHash::of_file(&fixture.old).expect("hash old");
    let released_hash = FileHash::of_file(&fixture.new).expect("hash new");
    assert_ne!(
        old_hash, released_hash,
        "fixture versions are identical, so every assertion below would be vacuous"
    );

    let server = TestServer::start();
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
            installer_url: &server.url("/app_1.0.1.AppImage"),
            patch_url: &server.url("/patch.zst"),
            patch_out: &patch,
            notes: None,
            pub_date: None,
        },
        &key,
        None,
    )
    .expect("release should build");

    server.serve(
        "/manifest.json",
        manifest.to_json().expect("serialise").into_bytes(),
    );
    server.serve("/patch.zst", read(&patch));
    server.serve("/app_1.0.1.AppImage", read(&fixture.new));

    World {
        pubkey: pubkey_b64(pair),
        base: fixture.old,
        released_hash,
        signature: manifest.platforms[PLATFORM].signature.clone(),
        manifest_json: manifest.to_json().expect("serialise"),
        server,
    }
}

impl World {
    /// The identity Tauri's own check would have produced from the document
    /// this server serves at `/manifest.json`.
    ///
    /// The manifest endpoint is still served, because in the real app Tauri
    /// fetches it — but the flow under test never does, which is what
    /// `the_flow_never_requests_the_manifest` asserts.
    fn identity(&self, current_version: &str) -> UpdateIdentity {
        UpdateIdentity::new(
            current_version,
            "1.0.1",
            PLATFORM,
            self.server.url("/app_1.0.1.AppImage"),
            &self.signature,
            &self.manifest_json,
        )
    }
}

fn run(
    w: &World,
    handoff: &RecordingHandoff,
    work: &Path,
    base: Option<&Path>,
) -> tauri_plugin_updater_delta::Result<Outcome> {
    let fetch = HttpFetch::new().expect("build http client");
    run_update(
        &w.identity("1.0.0"),
        &Context {
            pubkey: &w.pubkey,
            base,
            work_dir: work,
        },
        &fetch,
        handoff,
    )
}

#[test]
fn a_delta_update_over_real_http_installs_the_released_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let handoff = RecordingHandoff::default();

    let outcome =
        run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");

    assert!(
        matches!(outcome, Outcome::InstalledFromDelta { .. }),
        "expected a delta install, got {outcome:?}"
    );
    handoff.assert_installed_exactly(&w.released_hash);
}

#[test]
fn a_full_download_over_real_http_installs_the_released_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let handoff = RecordingHandoff::default();

    let outcome = run(&w, &handoff, &dir.path().join("work"), None).expect("should update");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    handoff.assert_installed_exactly(&w.released_hash);
}

#[test]
fn a_corrupt_patch_falls_back_and_still_installs_the_released_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    w.server
        .set("/patch.zst", Route::Body(b"not a patch at all".to_vec()));
    let handoff = RecordingHandoff::default();

    let outcome =
        run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    handoff.assert_installed_exactly(&w.released_hash);
}

#[test]
fn a_truncated_patch_download_falls_back_and_still_installs_the_released_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    // Headers promise the full length; the connection dies halfway. Only a real
    // socket can produce this — a fake Fetch cannot.
    let patch = read(&dir.path().join("patch.zst"));
    w.server.set("/patch.zst", Route::Truncated(patch));
    let handoff = RecordingHandoff::default();

    let outcome =
        run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    handoff.assert_installed_exactly(&w.released_hash);
}

#[test]
fn an_unreachable_patch_server_falls_back_and_still_installs_the_released_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    w.server.remove("/patch.zst");
    let handoff = RecordingHandoff::default();

    let outcome =
        run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    handoff.assert_installed_exactly(&w.released_hash);
}

#[test]
fn a_server_error_on_the_patch_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    w.server.set("/patch.zst", Route::Status(503));
    let handoff = RecordingHandoff::default();

    let outcome =
        run(&w, &handoff, &dir.path().join("work"), Some(&w.base)).expect("should update");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    handoff.assert_installed_exactly(&w.released_hash);
}

#[test]
fn a_wrong_base_version_falls_back_and_still_installs_the_released_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let wrong = dir.path().join("wrong_base.AppImage");
    std::fs::write(&wrong, b"an installer from some other release").expect("write");
    let handoff = RecordingHandoff::default();

    let outcome = run(&w, &handoff, &dir.path().join("work"), Some(&wrong)).expect("should update");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    handoff.assert_installed_exactly(&w.released_hash);
}

#[test]
fn a_signature_failure_does_not_fall_back_and_installs_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    w.pubkey = pubkey_b64(&keypair()); // app trusts a different key
    let handoff = RecordingHandoff::default();

    let result = run(&w, &handoff, &dir.path().join("work"), Some(&w.base));

    // DECISIONS #11: a signature failure is a fault of unknown origin, and the
    // fallback target is described by the same unauthenticated document. Retrying
    // there grants a second attempt rather than a safer one, so loud failure is
    // the only honest outcome. (Note the document is not signed, so this does not
    // prove it forged — the older wording here claimed that and was wrong.)
    assert!(
        matches!(result, Err(Error::Signature(_))),
        "expected a loud signature failure, got {result:?}"
    );
    handoff.assert_nothing_installed();
}

#[test]
fn a_tampered_full_download_installs_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    w.server.set(
        "/app_1.0.1.AppImage",
        Route::Body(b"a malicious installer".to_vec()),
    );
    let handoff = RecordingHandoff::default();

    let result = run(&w, &handoff, &dir.path().join("work"), None);

    assert!(matches!(result, Err(Error::Signature(_))));
    handoff.assert_nothing_installed();
}

#[test]
fn falling_back_does_not_lower_the_bar() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    // Patch broken *and* the artifact tampered: the delta path fails, the
    // fallback runs, and the fallback's own signature check catches it.
    w.server.set("/patch.zst", Route::Body(b"junk".to_vec()));
    w.server.set(
        "/app_1.0.1.AppImage",
        Route::Body(b"a malicious installer".to_vec()),
    );
    let handoff = RecordingHandoff::default();

    let result = run(&w, &handoff, &dir.path().join("work"), Some(&w.base));

    assert!(matches!(result, Err(Error::Signature(_))));
    handoff.assert_nothing_installed();
}

#[test]
fn the_flow_never_requests_the_manifest() {
    // Gate A test 10, over a real socket and stated as bluntly as it can be:
    // take the manifest endpoint off the server entirely and the update still
    // succeeds, because the flow reads the document Tauri already fetched.
    //
    // Under the old two-fetch architecture this test was
    // `an_unreachable_manifest_installs_nothing` and asserted `Err(Fetch)`. That
    // it now asserts the opposite is the whole point of the change: there is no
    // second request left to fail.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    w.server.remove("/manifest.json");
    let handoff = RecordingHandoff::default();

    let outcome = run(&w, &handoff, &dir.path().join("work"), Some(&w.base))
        .expect("the manifest is not fetched here, so removing it changes nothing");

    assert!(
        matches!(outcome, Outcome::InstalledFromDelta { .. }),
        "expected a delta install, got {outcome:?}"
    );
    handoff.assert_installed_exactly(&w.released_hash);
}
