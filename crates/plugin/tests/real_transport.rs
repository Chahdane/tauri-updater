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
use tauri_plugin_updater_delta::{Error, HttpFetch, HttpFetchBuilder};
use tauri_updater_delta_core::{FileHash, Limits, UpdateIdentity, VerifiedArtifact};
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

/// A fetcher configured for a localhost test server.
///
/// The opt-in is explicit rather than relying on `debug_assertions` allowing
/// plain HTTP, so these tests assert the same thing whichever profile they are
/// compiled in — and so the suite exercises the same opt-in a developer uses.
fn test_fetch() -> HttpFetchBuilder {
    HttpFetch::builder().dangerous_insecure_transport_protocol(true)
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
            tar_layer: None,
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
    let fetch = test_fetch().build().expect("build http client");
    run_update(
        &w.identity("1.0.0"),
        &Context {
            pubkey: &w.pubkey,
            base,
            cache: None,
            work_dir: work,
            limits: Limits::default(),
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

// ---- Gate B: transport policy, over real sockets ------------------------
//
// Every one of these describes something a server chooses and we must bound.
// They use HttpFetch directly rather than the whole flow, because what is under
// test is the transport policy itself.

use std::time::{Duration, Instant};
use tauri_updater_delta_core::client::Fetch;

/// A bare server and a scratch path, for tests that need no release at all.
fn transport_fixture() -> (TestServer, tempfile::TempDir) {
    (TestServer::start(), tempfile::tempdir().expect("temp dir"))
}

#[test]
fn a_plain_http_url_is_refused_without_the_opt_in() {
    // The policy this whole gate turns on. In a debug build upstream warns and
    // allows, so assert the arm that matches the profile — the same shape as the
    // unit test, but here against a real socket and the real client.
    let (server, dir) = transport_fixture();
    server.serve("/thing", b"hello".to_vec());

    let strict = HttpFetch::new().expect("build");
    let result = strict.fetch(&server.url("/thing"), &dir.path().join("out"));

    if cfg!(debug_assertions) {
        assert!(
            result.is_ok(),
            "development builds allow http, as upstream does"
        );
    } else {
        let err = result.expect_err("release builds must refuse plain http");
        assert!(
            err.contains("dangerous_insecure_transport_protocol"),
            "the refusal must name the opt-in: {err}"
        );
    }
}

#[test]
fn the_opt_in_makes_plain_http_work_in_any_profile() {
    let (server, dir) = transport_fixture();
    server.serve("/thing", b"hello".to_vec());
    let out = dir.path().join("out");

    test_fetch()
        .build()
        .expect("build")
        .fetch(&server.url("/thing"), &out)
        .expect("the opt-in permits http");

    assert_eq!(std::fs::read(&out).expect("read"), b"hello");
}

#[test]
fn a_redirect_is_followed_within_the_budget() {
    // Real hosting redirects: GitHub Releases answers with a 302 to
    // objects.githubusercontent.com. Refusing redirects outright would break
    // the most common way to publish an artifact, so the budget must be usable.
    let (server, dir) = transport_fixture();
    server.serve("/final", b"arrived".to_vec());
    server.set("/start", Route::Redirect(server.url("/final")));
    let out = dir.path().join("out");

    test_fetch()
        .max_redirects(5)
        .build()
        .expect("build")
        .fetch(&server.url("/start"), &out)
        .expect("one hop is well within budget");

    assert_eq!(std::fs::read(&out).expect("read"), b"arrived");
}

#[test]
fn a_chain_longer_than_the_budget_is_refused() {
    // A finite chain rather than a self-loop, deliberately. A loop would also
    // prove the point, but with the budget removed the test would hang instead
    // of failing — and a test that hangs under mutation cannot be used as
    // evidence that the guard is load-bearing.
    let (server, dir) = transport_fixture();
    const HOPS: usize = 8;
    server.serve("/hop8", b"should never be reached".to_vec());
    for i in 0..HOPS {
        server.set(
            &format!("/hop{i}"),
            Route::Redirect(server.url(&format!("/hop{}", i + 1))),
        );
    }
    let out = dir.path().join("out");

    let err = test_fetch()
        .max_redirects(3)
        .build()
        .expect("build")
        .fetch(&server.url("/hop0"), &out)
        .expect_err("a chain past the budget must be refused");

    assert!(
        err.contains("redirect"),
        "the error should name the redirect budget: {err}"
    );
    assert!(!out.exists(), "nothing may be written for a refused chain");
}

#[test]
fn a_stalled_server_times_out_rather_than_hanging() {
    // Headers arrive, a body is promised, and nothing else ever comes. No status
    // code expresses this and no connect timeout catches it: the connection was
    // established successfully. Only a request-wide deadline ends it.
    let (server, dir) = transport_fixture();
    server.set("/stall", Route::Stall);

    let started = Instant::now();
    let err = test_fetch()
        .request_timeout(Duration::from_secs(2))
        .build()
        .expect("build")
        .fetch(&server.url("/stall"), &dir.path().join("out"))
        .expect_err("a stalled response must not hang forever");
    let elapsed = started.elapsed();

    // Both bounds matter. Too fast would mean something else failed and the
    // timeout was never exercised; too slow would mean it did not fire.
    assert!(
        elapsed >= Duration::from_secs(1),
        "returned in {elapsed:?} — too fast to have been the timeout"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "took {elapsed:?} — the timeout did not fire"
    );
    assert!(
        err.to_lowercase().contains("timed out"),
        "the error must say it timed out, or a maintainer will hunt for a corrupt \
         artifact instead of a silent server: {err}"
    );
}

#[test]
fn an_oversized_content_length_is_refused_before_the_body_is_read() {
    // The cheap version of the attack: claim an enormous body and see whether
    // the client acts on the number.
    let (server, dir) = transport_fixture();
    server.set("/huge", Route::OversizedDeclared(50 * 1024 * 1024 * 1024));
    let out = dir.path().join("out");

    let err = test_fetch()
        .max_response_bytes(1024)
        .build()
        .expect("build")
        .fetch(&server.url("/huge"), &out)
        .expect_err("a declared length over the cap must be refused");

    assert!(err.contains("declares"), "unexpected error: {err}");
    assert!(!out.exists(), "nothing may be left at the destination");
}

#[test]
fn an_endless_chunked_body_is_cut_off_at_the_cap() {
    // The same attack with the declared length removed. A Content-Length check
    // alone would let this run until the disk filled, which is exactly why the
    // streaming counter exists.
    let (server, dir) = transport_fixture();
    server.set("/endless", Route::EndlessChunked);
    let out = dir.path().join("out");

    let err = test_fetch()
        .max_response_bytes(256 * 1024)
        .request_timeout(Duration::from_secs(30))
        .build()
        .expect("build")
        .fetch(&server.url("/endless"), &out)
        .expect_err("an unbounded body must be cut off");

    assert!(
        err.contains("exceeded"),
        "expected the streaming cap to fire, got: {err}"
    );
    assert!(!out.exists(), "nothing may be left at the destination");
}

#[test]
fn a_failed_download_leaves_nothing_at_the_destination() {
    // The property that matters beyond any single failure mode: whatever goes
    // wrong, no half-file is left under a name a later step could mistake for a
    // finished download.
    let (server, dir) = transport_fixture();
    server.set("/truncated", Route::Truncated(vec![b'x'; 4096]));
    let out = dir.path().join("out.artifact");

    let result = test_fetch()
        .build()
        .expect("build")
        .fetch(&server.url("/truncated"), &out);

    assert!(result.is_err(), "a truncated body must be an error");
    assert!(!out.exists(), "no artifact may survive a failed download");
    assert!(
        !out.with_extension("part").exists(),
        "the partial file must be cleaned up too"
    );
}

#[test]
fn a_failed_download_does_not_destroy_the_file_it_would_have_replaced() {
    // What the .part file actually buys, which "nothing is left behind" does not
    // distinguish: writing straight to the destination and deleting it on error
    // also leaves nothing behind — but it has already truncated whatever was
    // there. A cached artifact from a previous update is exactly what sits at
    // that path, and losing it turns a failed download into a lost base.
    let (server, dir) = transport_fixture();
    server.set("/truncated", Route::Truncated(vec![b'x'; 8192]));

    let out = dir.path().join("cached.artifact");
    std::fs::write(&out, b"the artifact we already had").expect("seed the cache");

    let result = test_fetch()
        .build()
        .expect("build")
        .fetch(&server.url("/truncated"), &out);

    assert!(result.is_err(), "a truncated body must be an error");
    assert_eq!(
        std::fs::read(&out).expect("the previous artifact must survive"),
        b"the artifact we already had",
        "a failed download overwrote the file it was meant to replace"
    );
}
