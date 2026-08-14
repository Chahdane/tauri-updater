//! The authenticated release identity, exercised through the whole client flow.
//!
//! Every release here is signed by a real key with a real trusted comment, and
//! every attack is carried out on the bytes a server would actually serve. The
//! point is not that the parser rejects bad strings — `release_identity.rs`
//! covers that — but that a **contradiction reaches no installer and never
//! decays into a full download**.
//!
//! # The distinction these tests exist to police
//!
//! | Condition | Correct behaviour |
//! | --- | --- |
//! | Signature carries no binding at all | Delta paths unavailable, **Full allowed** |
//! | Signature carries a binding that contradicts the release | **Fail closed**, no Full |
//!
//! Getting that backwards in either direction is a real bug. Treating a legacy
//! release as an attack breaks every project still signing with
//! `tauri signer sign`; treating a contradiction as a compatibility condition
//! hands the attacker the fallback path.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use base64::Engine as _;
use minisign::KeyPair;
use tauri_plugin_updater_delta::test_support::{run_update, Context, InstallHandoff, Outcome};
use tauri_plugin_updater_delta::Error;
use tauri_updater_delta_core::client::Fetch;
use tauri_updater_delta_core::release_identity::{current_platform, ReleaseIdentity};
use tauri_updater_delta_core::{FileHash, Limits, Refusal, UpdateIdentity, VerifiedArtifact};

const APP_ID: &str = "dev.example.testapp";
const OTHER_APP: &str = "dev.example.otherapp";
const INSTALLER_URL: &str = "https://example.com/app_1.0.1.bin";

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

/// A release: bytes, a key, and whatever identity we choose to sign over them.
struct Release {
    key: KeyPair,
    bytes: Vec<u8>,
    pubkey: String,
}

impl Release {
    fn new(body: &str) -> Self {
        let key = KeyPair::generate_encrypted_keypair(Some(String::new())).expect("keypair");
        let pubkey = base64::engine::general_purpose::STANDARD
            .encode(key.pk.to_box().expect("box pk").into_string());
        Self {
            key,
            bytes: body.as_bytes().to_vec(),
            pubkey,
        }
    }

    fn honest_identity(&self, version: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            app_id: APP_ID.to_owned(),
            version: version.to_owned(),
            platform: current_platform(),
            representation: "opaque-v1".to_owned(),
            artifact_blake3: FileHash::of_bytes(&self.bytes).to_hex(),
            artifact_size: self.bytes.len() as u64,
            signed_at: 1_786_637_312,
        }
    }

    /// Sign the bytes, carrying `comment` as the trusted comment.
    fn sign_with(&self, comment: Option<&str>) -> String {
        base64::engine::general_purpose::STANDARD.encode(
            minisign::sign(None, &self.key.sk, &self.bytes[..], comment, None)
                .expect("sign")
                .into_string(),
        )
    }

    fn sign(&self, identity: &ReleaseIdentity) -> String {
        self.sign_with(Some(&identity.to_trusted_comment()))
    }

    /// What an older toolchain produces: valid, and saying nothing.
    fn sign_legacy(&self) -> String {
        self.sign_with(None)
    }
}

/// A minimal Tauri-superset manifest with no delta layer.
///
/// No delta layer keeps these tests about the identity gate: the only paths
/// available are Full and Refused, so a "Full" result cannot be a delta that
/// quietly fell back.
fn manifest(version: &str, signature: &str) -> String {
    format!(
        r#"{{"version":"{version}","platforms":{{"{plat}":{{"url":"{INSTALLER_URL}","signature":"{signature}"}}}}}}"#,
        plat = current_platform()
    )
}

struct Outcome_<'a> {
    result: tauri_plugin_updater_delta::Result<Outcome>,
    handoff: &'a RecordingHandoff,
}

/// Run one update with `signature` in both the manifest and Tauri's selection.
fn run(
    dir: &Path,
    release: &Release,
    served_version: &str,
    signature: &str,
    handoff: &RecordingHandoff,
) -> tauri_plugin_updater_delta::Result<Outcome> {
    let server = FakeServer(RefCell::new(HashMap::from([(
        INSTALLER_URL.to_owned(),
        release.bytes.clone(),
    )])));
    let identity = UpdateIdentity::new(
        "1.0.0",
        served_version,
        "darwin",
        INSTALLER_URL,
        signature,
        manifest(served_version, signature),
    );
    run_update(
        &identity,
        &Context {
            pubkey: &release.pubkey,
            base: None,
            cache: None,
            app_id: APP_ID,
            work_dir: dir,
            limits: Limits::default(),
        },
        &server,
        handoff,
    )
}

fn assert_failed_closed(outcome: Outcome_<'_>, what: &str) {
    match &outcome.result {
        Err(Error::Refused(Refusal::ReleaseIdentity { .. })) => {}
        other => panic!("{what}: expected a release-identity refusal, got {other:?}"),
    }
    assert!(
        outcome.handoff.installed.borrow().is_empty(),
        "{what}: nothing may be installed"
    );
}

// ---- the honest case -----------------------------------------------------

#[test]
fn an_honest_authenticated_release_installs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the 1.0.1 installer");
    let signature = release.sign(&release.honest_identity("1.0.1"));
    let handoff = RecordingHandoff::default();

    let outcome = run(dir.path(), &release, "1.0.1", &signature, &handoff)
        .expect("an honest authenticated release must install");
    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert_eq!(handoff.installed.borrow()[0], release.bytes);
}

// ---- the attack this whole gate exists for -------------------------------

#[test]
fn an_old_signed_artifact_relabelled_as_newer_fails_closed() {
    // The Audit #2 attack, end to end. The attacker has no key. They take a
    // genuinely signed 1.0.0 release and serve it as 9.9.9. The artifact
    // signature verifies — those bytes really were signed — and before this
    // gate nothing else disagreed.
    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the genuinely signed 1.0.0 installer");
    let signature = release.sign(&release.honest_identity("1.0.0"));
    let handoff = RecordingHandoff::default();

    let result = run(dir.path(), &release, "9.9.9", &signature, &handoff);
    assert_failed_closed(
        Outcome_ {
            result,
            handoff: &handoff,
        },
        "relabelled release",
    );
}

#[test]
fn editing_the_version_inside_the_signature_breaks_verification() {
    // The other half: rather than relabelling in the manifest, edit the signed
    // comment itself. This must not even reach the identity comparison — the
    // global signature covers the comment, so the cryptography refuses first.
    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the genuinely signed 1.0.0 installer");
    let honest = release.sign(&release.honest_identity("1.0.0"));

    let decoded = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(&honest)
            .expect("decode"),
    )
    .expect("utf8");
    let tampered =
        base64::engine::general_purpose::STANDARD.encode(decoded.replace("v:1.0.0", "v:9.9.9"));

    let handoff = RecordingHandoff::default();
    let result = run(dir.path(), &release, "9.9.9", &tampered, &handoff);
    assert!(result.is_err(), "got {result:?}");
    assert!(handoff.installed.borrow().is_empty());
}

#[test]
fn a_trusted_comment_spliced_from_another_release_fails_verification() {
    // Take release A's artifact signature line and release B's trusted comment.
    // The global signature is over `artifact_sig || trusted_comment`, so the
    // pair cannot be recombined without the key.
    let dir = tempfile::tempdir().expect("temp dir");
    let a = Release::new("release A bytes");
    let a_sig = a.sign(&a.honest_identity("1.0.0"));

    // B is signed by the same key, so key-id checks cannot be what refuses it.
    let mut b_bytes = a.bytes.clone();
    b_bytes.extend_from_slice(b" plus more");
    let b = Release {
        key: KeyPair::generate_encrypted_keypair(Some(String::new())).expect("keypair"),
        bytes: b_bytes,
        pubkey: a.pubkey.clone(),
    };
    let b_sig = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(
            None,
            &a.key.sk,
            &b.bytes[..],
            Some(&b.honest_identity("9.9.9").to_trusted_comment()),
            None,
        )
        .expect("sign")
        .into_string(),
    );

    let decode = |s: &str| {
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(s)
                .expect("decode"),
        )
        .expect("utf8")
    };
    let mut mine: Vec<String> = decode(&a_sig).lines().map(str::to_owned).collect();
    let theirs: Vec<String> = decode(&b_sig).lines().map(str::to_owned).collect();
    mine[2] = theirs[2].clone();
    let spliced = base64::engine::general_purpose::STANDARD.encode(mine.join("\n") + "\n");

    let handoff = RecordingHandoff::default();
    let result = run(dir.path(), &a, "9.9.9", &spliced, &handoff);
    assert!(
        result.is_err(),
        "a spliced comment must not verify: {result:?}"
    );
    assert!(handoff.installed.borrow().is_empty());
}

// ---- every field, one at a time ------------------------------------------

#[test]
fn each_authenticated_field_fails_closed_when_it_disagrees() {
    type Case = (
        &'static str,
        Box<dyn Fn(&Release) -> (ReleaseIdentity, String)>,
    );
    let cases: Vec<Case> = vec![
        (
            "version",
            Box::new(|r: &Release| {
                let mut id = r.honest_identity("1.0.1");
                id.version = "1.0.2".to_owned();
                (id, "1.0.1".to_owned())
            }),
        ),
        (
            "blake3",
            Box::new(|r: &Release| {
                let mut id = r.honest_identity("1.0.1");
                id.artifact_blake3 = FileHash::of_bytes(b"different bytes").to_hex();
                (id, "1.0.1".to_owned())
            }),
        ),
        (
            "size",
            Box::new(|r: &Release| {
                let mut id = r.honest_identity("1.0.1");
                id.artifact_size += 1;
                (id, "1.0.1".to_owned())
            }),
        ),
        (
            "platform",
            Box::new(|r: &Release| {
                let mut id = r.honest_identity("1.0.1");
                id.platform = "plan9-vax".to_owned();
                (id, "1.0.1".to_owned())
            }),
        ),
        (
            "app",
            Box::new(|r: &Release| {
                let mut id = r.honest_identity("1.0.1");
                id.app_id = OTHER_APP.to_owned();
                (id, "1.0.1".to_owned())
            }),
        ),
    ];

    for (field, mutate) in cases {
        let dir = tempfile::tempdir().expect("temp dir");
        let release = Release::new("the 1.0.1 installer");
        let (identity, served) = mutate(&release);
        let signature = release.sign(&identity);
        let handoff = RecordingHandoff::default();

        let result = run(dir.path(), &release, &served, &signature, &handoff);
        assert_failed_closed(
            Outcome_ {
                result,
                handoff: &handoff,
            },
            field,
        );
    }
}

#[test]
fn an_artifact_for_another_application_signed_with_the_same_key_is_refused() {
    // The reason app identity is bound at all: one organisation, one signing
    // key, two products. Every other field can be honest and the artifact still
    // belongs to something else.
    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the other product's installer");
    let mut identity = release.honest_identity("1.0.1");
    identity.app_id = OTHER_APP.to_owned();
    let signature = release.sign(&identity);
    let handoff = RecordingHandoff::default();

    let result = run(dir.path(), &release, "1.0.1", &signature, &handoff);
    assert_failed_closed(
        Outcome_ {
            result,
            handoff: &handoff,
        },
        "cross-application artifact",
    );
}

// ---- malformed and future protocols --------------------------------------

#[test]
fn a_malformed_authenticated_identity_fails_closed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the 1.0.1 installer");
    let handoff = RecordingHandoff::default();

    // Signed, so the cryptography is fine. The statement is not.
    let signature = release.sign_with(Some("delta-v1 app:dev.example.testapp v:not-semver"));
    let result = run(dir.path(), &release, "1.0.1", &signature, &handoff);
    assert_failed_closed(
        Outcome_ {
            result,
            handoff: &handoff,
        },
        "malformed identity",
    );
}

#[test]
fn an_unsupported_protocol_version_fails_closed_rather_than_reading_as_legacy() {
    // The dangerous mistake: treating anything unparseable as "no binding",
    // which is the attacker's preferred outcome because it downgrades a bound
    // release to the legacy rules.
    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the 1.0.1 installer");
    let handoff = RecordingHandoff::default();

    let signature = release.sign_with(Some(
        "delta-v2 app:dev.example.testapp v:1.0.1 plat:x-y rep:opaque-v1 b3:00 sz:1 ts:1",
    ));
    let result = run(dir.path(), &release, "1.0.1", &signature, &handoff);
    assert_failed_closed(
        Outcome_ {
            result,
            handoff: &handoff,
        },
        "future protocol",
    );
}

// ---- legacy compatibility ------------------------------------------------

#[test]
fn a_legacy_signature_still_installs_through_the_full_path() {
    // What `tauri signer sign` produces today. Valid, and saying nothing about
    // which release it is. That is a compatibility condition, not an attack, so
    // the full path must keep working exactly as a stock Tauri client's would.
    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the 1.0.1 installer");
    let signature = release.sign_legacy();
    let handoff = RecordingHandoff::default();

    let outcome = run(dir.path(), &release, "1.0.1", &signature, &handoff)
        .expect("a legacy release must still install in full");
    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert_eq!(handoff.installed.borrow()[0], release.bytes);
}

#[test]
fn a_legacy_signature_makes_a_published_delta_unavailable() {
    // A real release with a real delta layer and a real base, whose signature
    // was then replaced with a legacy one -- exactly what a project signing with
    // `tauri signer sign` produces. Without the delta layer this test would be
    // ambiguous: Full would happen anyway because nothing was published.
    use tauri_updater_delta_core::client::{plan_update, PlanContext, UpdateSource};
    use tauri_updater_delta_release::signing::SigningKey;
    use tauri_updater_delta_release::{build_release, Predecessor, ReleaseRequest};

    let dir = tempfile::tempdir().expect("temp dir");
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir.path());
    let patch = dir.path().join("patch.zst");

    let pair = KeyPair::generate_encrypted_keypair(Some(String::new())).expect("keypair");
    let pubkey = base64::engine::general_purpose::STANDARD
        .encode(pair.pk.to_box().expect("box pk").into_string());
    let key = SigningKey::from_str(&pair.sk.to_box(None).expect("box").into_string(), None)
        .expect("load key");

    let (mut manifest, _) = build_release(
        &ReleaseRequest {
            platform: &current_platform(),
            version: "1.0.1",
            new_installer: &fixture.new,
            installer_url: INSTALLER_URL,
            notes: None,
            pub_date: None,
            app_id: APP_ID,
            predecessor: Some(Predecessor {
                from_version: "1.0.0",
                installer: &fixture.old,
                patch_url: "https://example.com/patch.zst",
                patch_out: &patch,
                tar_layer: None,
            }),
        },
        &key,
        None,
    )
    .expect("release should build");

    // Sanity: with the authenticated signature the delta path is taken. Without
    // this, "Full" below could mean anything.
    let server = FakeServer(RefCell::new(HashMap::from([(
        "https://example.com/patch.zst".to_owned(),
        std::fs::read(&patch).expect("read patch"),
    )])));
    let plan = |manifest: &tauri_updater_delta_core::Manifest, server: &FakeServer| {
        let signature = manifest.platforms[&current_platform()].signature.clone();
        let identity = UpdateIdentity::new(
            "1.0.0",
            "1.0.1",
            "darwin",
            INSTALLER_URL,
            &signature,
            manifest.to_json().expect("serialise"),
        );
        plan_update(
            &identity,
            &PlanContext {
                base: Some(&fixture.old),
                cache: None,
                pubkey: &pubkey,
                app_id: APP_ID,
                work_dir: &dir.path().join("work"),
                limits: Limits::default(),
            },
            server,
        )
    };
    assert!(
        matches!(plan(&manifest, &server), UpdateSource::Delta { .. }),
        "the fixture must reach the delta path when the signature is bound"
    );

    // Now the same release signed the old way: valid, and saying nothing.
    let legacy = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(
            None,
            &pair.sk,
            std::fs::File::open(&fixture.new).expect("open"),
            None,
            None,
        )
        .expect("sign")
        .into_string(),
    );
    let platform = current_platform();
    manifest.platforms.get_mut(&platform).unwrap().signature = legacy.clone();
    manifest
        .delta
        .as_mut()
        .unwrap()
        .platforms
        .get_mut(&platform)
        .unwrap()
        .signature = legacy;

    let source = plan(&manifest, &server);
    let UpdateSource::Full {
        reason, attempted, ..
    } = source
    else {
        panic!("expected a full download, got {source:?}");
    };
    assert!(
        !attempted.tar_delta && !attempted.direct_delta,
        "neither delta path may be attempted without a binding"
    );
    assert!(
        reason
            .map(|r| r.to_string().contains("no authenticated release identity"))
            .unwrap_or(false),
        "the reason must name the missing binding"
    );
}

// ---- the two must not be confused ---------------------------------------

#[test]
fn an_authenticated_contradiction_never_becomes_a_full_download() {
    // The single most important property here. A contradiction is not a delta
    // failure, and the full path is pointed at the same artifact by the same
    // document — so falling back would run the contradiction down the other
    // branch. The manifest below has no delta layer at all, so Full is the only
    // other thing that could possibly happen.
    use tauri_updater_delta_core::client::{plan_update, PlanContext, UpdateSource};

    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the genuinely signed 1.0.0 installer");
    let signature = release.sign(&release.honest_identity("1.0.0"));
    let server = FakeServer(RefCell::new(HashMap::new()));

    let identity = UpdateIdentity::new(
        "1.0.0",
        "9.9.9",
        "darwin",
        INSTALLER_URL,
        &signature,
        manifest("9.9.9", &signature),
    );
    let source = plan_update(
        &identity,
        &PlanContext {
            base: None,
            cache: None,
            pubkey: &release.pubkey,
            app_id: APP_ID,
            work_dir: dir.path(),
            limits: Limits::default(),
        },
        &server,
    );

    assert!(
        matches!(
            source,
            UpdateSource::Refused {
                reason: Refusal::ReleaseIdentity { .. }
            }
        ),
        "a contradiction must refuse, not fall back: {source:?}"
    );
}

#[test]
fn a_genuine_old_release_keeps_its_true_version_and_is_refused_as_a_downgrade() {
    // What the binding does *not* fix, asserted so the limit is visible in the
    // suite rather than only in prose. Serving a real older release is still
    // possible; it now arrives honestly labelled, and the downgrade policy is
    // what refuses it.
    let dir = tempfile::tempdir().expect("temp dir");
    let release = Release::new("the genuinely signed 1.0.0 installer");
    let signature = release.sign(&release.honest_identity("1.0.0"));
    let handoff = RecordingHandoff::default();

    let server = FakeServer(RefCell::new(HashMap::from([(
        INSTALLER_URL.to_owned(),
        release.bytes.clone(),
    )])));
    // Installed 2.0.0, offered a real, honestly-labelled 1.0.0.
    let identity = UpdateIdentity::new(
        "2.0.0",
        "1.0.0",
        "darwin",
        INSTALLER_URL,
        &signature,
        manifest("1.0.0", &signature),
    );
    let result = run_update(
        &identity,
        &Context {
            pubkey: &release.pubkey,
            base: None,
            cache: None,
            app_id: APP_ID,
            work_dir: dir.path(),
            limits: Limits::default(),
        },
        &server,
        &handoff,
    );

    assert!(
        matches!(result, Err(Error::Refused(Refusal::Downgrade { .. }))),
        "expected a downgrade refusal, got {result:?}"
    );
    assert!(handoff.installed.borrow().is_empty());
}
