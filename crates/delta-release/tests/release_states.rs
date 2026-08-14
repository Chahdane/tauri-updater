//! The three states a release can be in, and the gate that guards all of them.
//!
//! # Why these are tests rather than a runbook
//!
//! A release workflow runs once per release, on a machine nobody is watching,
//! and its failure modes are discovered by users. Blocker B5 is the canonical
//! example: the first-release path had never been executed by anything, so a
//! condition the workflow *handled* — no previous tag — silently produced an
//! artifact with no updater document and no signature.
//!
//! | State | Predecessor | Must publish |
//! | --- | --- | --- |
//! | **A** | none | signed Full-only manifest |
//! | **B** | exists, unusable for delta | signed Full-only manifest |
//! | **C** | exists and is usable | Full **and** delta metadata |
//!
//! None of the three is a failure. Two of them used to be indistinguishable from
//! one, because "no delta" and "no release document" went down the same branch.
//!
//! Every case here ends at [`verify_release`], because producing a manifest and
//! producing a *publishable* manifest are different claims and only the second
//! one matters.

use std::path::{Path, PathBuf};

use minisign::KeyPair;
use tauri_updater_delta_core::manifest::Manifest;
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::verify::{verify_release, ReleaseUnderTest};
use tauri_updater_delta_release::{
    build_release, prove_patch_reconstructs, Predecessor, ReleaseRequest, TarLayerOptions,
};

const PLATFORM: &str = "linux-x86_64";
const APP_ID: &str = "dev.example.testapp";
const BASE: &str = "https://releases.example.com";

fn keypair() -> KeyPair {
    KeyPair::generate_encrypted_keypair(Some(String::new())).expect("generate keypair")
}

fn signing_key(pair: &KeyPair) -> SigningKey {
    SigningKey::from_str(&pair.sk.to_box(None).expect("box key").into_string(), None)
        .expect("load key")
}

/// The public key in the form `tauri.conf.json` carries: base64 of the whole
/// minisign public-key box, not the bare key line.
fn public_key_base64(pair: &KeyPair) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .encode(pair.pk.to_box().expect("box public key").into_string())
}

/// Check a manifest the way the release workflow does, just before upload.
fn check(manifest: &Manifest, artifact: &Path, pair: &KeyPair, tag: &str) -> String {
    let report = verify_release(
        manifest,
        &ReleaseUnderTest {
            tag,
            app_id: APP_ID,
            platform: PLATFORM,
            artifact,
            pubkey: &public_key_base64(pair),
            allow_insecure_urls: false,
        },
    )
    .unwrap_or_else(|e| panic!("the produced manifest must be publishable: {e}"));
    report.identity
}

/// Assert the parts of a manifest Tauri itself needs, whatever else is present.
///
/// A release that publishes patches but no full-download entry is a release no
/// stock updater can use, so these are checked in every state rather than only
/// in the interesting one.
fn assert_tauri_can_use_it(manifest: &Manifest, version: &str) {
    assert_eq!(
        manifest.version, version,
        "the version Tauri compares against"
    );
    let entry = manifest
        .platforms
        .get(PLATFORM)
        .expect("Tauri looks up the platform key and finds nothing without this");
    assert!(
        entry.url.starts_with("https://"),
        "the full-download URL Tauri fetches: {}",
        entry.url
    );
    assert!(
        !entry.signature.is_empty(),
        "the signature Tauri verifies before installing"
    );
}

// ---- fixtures -----------------------------------------------------------

/// Two artifacts that differ enough to make a meaningful patch.
fn artifacts(dir: &Path) -> (PathBuf, PathBuf) {
    let old: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(2654435761) % 251) as u8)
        .collect();
    let mut new = old.clone();
    new[100_000..100_064].copy_from_slice(&[7u8; 64]);
    new.extend_from_slice(b"and a little more, so the sizes differ");

    let old_path = dir.join("app_1.0.0.bin");
    let new_path = dir.join("app_1.0.1.bin");
    std::fs::write(&old_path, &old).expect("write old");
    std::fs::write(&new_path, &new).expect("write new");
    (old_path, new_path)
}

// ---- state A ------------------------------------------------------------

#[test]
fn state_a_no_predecessor_still_publishes_a_complete_updater_release() {
    // Blocker B5. Before the fix this case could not be expressed: the request
    // type required a previous installer, so the workflow skipped the whole
    // step and published an artifact with no manifest and no signature.
    let dir = tempfile::tempdir().expect("temp dir");
    let (_old, new) = artifacts(dir.path());
    let pair = keypair();

    let (manifest, summary) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.0",
            new_installer: &new,
            installer_url: &format!("{BASE}/app_1.0.0.bin"),
            notes: Some("the first one"),
            pub_date: Some("2026-08-14T00:00:00Z"),
            app_id: APP_ID,
            predecessor: None,
        },
        &signing_key(&pair),
        None,
    )
    .expect("a first release must build");

    assert_tauri_can_use_it(&manifest, "1.0.0");

    // No patches, and specifically no *fabricated* ones. A delta entry naming a
    // patch that does not exist would send every client to a 404 before it fell
    // back, which is slower than having no delta layer at all.
    let has_patches = manifest
        .delta
        .as_ref()
        .and_then(|d| d.platforms.get(PLATFORM))
        .is_some_and(|e| !e.patches.is_empty());
    assert!(!has_patches, "a first release must publish no patches");
    assert_eq!(summary.patch_size, None);
    assert_eq!(summary.ratio_percent(), None);

    let identity = check(&manifest, &new, &pair, "v1.0.0");
    assert!(
        identity.starts_with("delta-v1 app:dev.example.testapp v:1.0.0 "),
        "a first release still carries an authenticated identity: {identity}"
    );
}

#[test]
fn state_a_survives_a_round_trip_through_json() {
    // The workflow writes the manifest, then a separate process reads it back to
    // check it. An in-memory value that serialises to something unusable would
    // pass every assertion above and fail in production.
    let dir = tempfile::tempdir().expect("temp dir");
    let (_old, new) = artifacts(dir.path());
    let pair = keypair();

    let (manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.0",
            new_installer: &new,
            installer_url: &format!("{BASE}/app_1.0.0.bin"),
            notes: None,
            pub_date: None,
            app_id: APP_ID,
            predecessor: None,
        },
        &signing_key(&pair),
        None,
    )
    .expect("build");

    let json = manifest.to_json().expect("serialise");
    let reread = Manifest::from_json(&json).expect("a published manifest must parse");
    assert_tauri_can_use_it(&reread, "1.0.0");
    check(&reread, &new, &pair, "v1.0.0");
}

// ---- state B ------------------------------------------------------------

#[test]
fn state_b_an_unusable_predecessor_still_publishes_a_complete_updater_release() {
    // The predecessor exists but cannot be patched from -- here because the
    // asset was never downloaded, which is what the workflow sees when a
    // previous release has no artifact for this platform. The caller's answer is
    // to pass no predecessor, and the result must be a normal Full-only release
    // rather than a failure.
    let dir = tempfile::tempdir().expect("temp dir");
    let (_old, new) = artifacts(dir.path());
    let pair = keypair();

    let (manifest, summary) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &new,
            installer_url: &format!("{BASE}/app_1.0.1.bin"),
            notes: None,
            pub_date: None,
            app_id: APP_ID,
            predecessor: None,
        },
        &signing_key(&pair),
        None,
    )
    .expect("an unusable predecessor is not a release failure");

    assert_tauri_can_use_it(&manifest, "1.0.1");
    assert_eq!(summary.patch_size, None);
    check(&manifest, &new, &pair, "v1.0.1");
}

#[test]
fn state_b_a_predecessor_that_does_not_exist_is_a_loud_failure_not_a_silent_one() {
    // The other half of state B, and the half that must NOT degrade. Being handed
    // a path that is not there means the caller believed it had a predecessor.
    // Publishing Full-only in that case would hide a broken download step behind
    // a release that looks fine.
    let dir = tempfile::tempdir().expect("temp dir");
    let (_old, new) = artifacts(dir.path());
    let pair = keypair();
    let patch = dir.path().join("p.zst");

    let err = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &new,
            installer_url: &format!("{BASE}/app_1.0.1.bin"),
            notes: None,
            pub_date: None,
            app_id: APP_ID,
            predecessor: Some(Predecessor {
                from_version: "1.0.0",
                installer: &dir.path().join("nothing-here.bin"),
                patch_url: &format!("{BASE}/p.zst"),
                patch_out: &patch,
                tar_layer: None,
            }),
        },
        &signing_key(&pair),
        None,
    )
    .expect_err("a missing predecessor file must fail rather than degrade");

    assert!(
        err.to_string().contains("does not exist"),
        "the error should name the problem: {err}"
    );
}

// ---- state C ------------------------------------------------------------

#[test]
fn state_c_a_usable_predecessor_publishes_full_and_delta() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (old, new) = artifacts(dir.path());
    let pair = keypair();
    let patch = dir.path().join("1.0.0-to-1.0.1.zst");

    let (manifest, summary) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &new,
            installer_url: &format!("{BASE}/app_1.0.1.bin"),
            notes: None,
            pub_date: None,
            app_id: APP_ID,
            predecessor: Some(Predecessor {
                from_version: "1.0.0",
                installer: &old,
                patch_url: &format!("{BASE}/1.0.0-to-1.0.1.zst"),
                patch_out: &patch,
                tar_layer: None,
            }),
        },
        &signing_key(&pair),
        None,
    )
    .expect("a normal release must build");

    // Everything state A publishes, plus the delta.
    assert_tauri_can_use_it(&manifest, "1.0.1");
    assert!(summary.patch_size.is_some());
    assert!(patch.is_file(), "the patch file must actually be written");

    let entry = manifest
        .delta
        .as_ref()
        .expect("delta layer")
        .platforms
        .get(PLATFORM)
        .expect("platform entry");
    assert!(
        entry.patches.contains_key("1.0.0"),
        "the upgrade path must be described"
    );

    let report = verify_release(
        &manifest,
        &ReleaseUnderTest {
            tag: "v1.0.1",
            app_id: APP_ID,
            platform: PLATFORM,
            artifact: &new,
            pubkey: &public_key_base64(&pair),
            allow_insecure_urls: false,
        },
    )
    .expect("publishable");
    assert_eq!(report.direct_patch_from, vec!["1.0.0".to_owned()]);
    assert!(report.tar_patch_from.is_empty(), "these are not tarballs");
}

#[test]
fn a_tar_layer_cannot_be_asked_for_without_a_predecessor() {
    // A type-level claim, checked because it is the kind of thing a later
    // refactor silently loosens: a tar patch is a patch between two artifacts,
    // so `TarLayerOptions` lives inside `Predecessor` and there is no way to
    // spell "tar layer, no predecessor".
    //
    // If this ever stops compiling as written, the shape has changed.
    let dir = tempfile::tempdir().expect("temp dir");
    let patch = dir.path().join("t.zst");
    let options = TarLayerOptions {
        patch_url: "https://example.com/t.zst",
        patch_out: &patch,
        work_dir: None,
        max_tar_bytes: 1024,
        required: false,
    };
    let pred = Predecessor {
        from_version: "1.0.0",
        installer: Path::new("/nonexistent"),
        patch_url: "https://example.com/p.zst",
        patch_out: &patch,
        tar_layer: Some(options),
    };
    assert!(pred.tar_layer.is_some());
}

// ---- what must never reach an upload ------------------------------------
//
// Gate P1 and Gate P2 protect the client. These protect the *publisher*, and
// they are a different set: a client refuses what it is served, while a release
// pipeline has to refuse what it is about to serve. Every case below produces a
// manifest that is internally consistent and would pass `Manifest::validate`.

/// A published release, with the pieces a test needs to tamper with.
struct Publishable {
    manifest: Manifest,
    artifact: PathBuf,
    pair: KeyPair,
    #[allow(dead_code)]
    dir: tempfile::TempDir,
}

fn publishable() -> Publishable {
    let dir = tempfile::tempdir().expect("temp dir");
    let (old, new) = artifacts(dir.path());
    let pair = keypair();
    let patch = dir.path().join("1.0.0-to-1.0.1.zst");

    let (manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &new,
            installer_url: &format!("{BASE}/app_1.0.1.bin"),
            notes: None,
            pub_date: None,
            app_id: APP_ID,
            predecessor: Some(Predecessor {
                from_version: "1.0.0",
                installer: &old,
                patch_url: &format!("{BASE}/1.0.0-to-1.0.1.zst"),
                patch_out: &patch,
                tar_layer: None,
            }),
        },
        &signing_key(&pair),
        None,
    )
    .expect("build");

    Publishable {
        manifest,
        artifact: new,
        pair,
        dir,
    }
}

/// Run the publish gate and require a refusal mentioning `expect`.
fn must_refuse(p: &Publishable, tag: &str, expect: &str) {
    let err = verify_release(
        &p.manifest,
        &ReleaseUnderTest {
            tag,
            app_id: APP_ID,
            platform: PLATFORM,
            artifact: &p.artifact,
            pubkey: &public_key_base64(&p.pair),
            allow_insecure_urls: false,
        },
    )
    .expect_err("this must not be publishable");

    let text = err.to_string();
    assert!(
        text.contains("refusing to publish"),
        "every gate failure is a refusal: {text}"
    );
    assert!(
        text.contains(expect),
        "expected the refusal to mention {expect:?}, got: {text}"
    );
}

#[test]
fn a_manifest_describing_a_different_tag_is_refused() {
    // The release tool is told the version; the tag is what users get. Two
    // sources of truth, and nothing else compares them.
    let p = publishable();
    must_refuse(&p, "v9.9.9", "the tag being published");
}

#[test]
fn an_identity_describing_a_different_artifact_is_refused() {
    // The signature verifies, the manifest is consistent, and the artifact on
    // disk is not the one the identity describes -- an asset swapped between
    // generation and upload.
    let mut p = publishable();
    let other = p.dir.path().join("something_else.bin");
    std::fs::write(&other, b"a different artifact entirely").expect("write");
    p.artifact = other;

    // The signature is over the original bytes, so this trips at verification
    // before the identity check -- which is the correct order, and still a
    // refusal.
    must_refuse(&p, "v1.0.1", "does not verify");
}

#[test]
fn a_delta_layer_describing_a_different_artifact_is_refused() {
    // The half a client would act on: the full-download entry is right, and the
    // delta layer names a digest that is not this artifact. A client rebuilding
    // from a patch would compare its work against the wrong number and fall back
    // forever, silently.
    let mut p = publishable();
    let entry = p
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(PLATFORM)
        .expect("platform");
    entry.target_installer_blake3 =
        tauri_updater_delta_core::FileHash::of_bytes(b"not this release").to_hex();

    must_refuse(&p, "v1.0.1", "the file being uploaded hashes to");
}

#[test]
fn an_artifact_without_an_authenticated_identity_is_refused() {
    // A legacy signature is fine for a *client* -- that is the approved
    // migration policy. It is not fine to publish from this tooling, which
    // always binds an identity, so its absence means the artifact was signed by
    // something else.
    let mut p = publishable();
    let bytes = std::fs::read(&p.artifact).expect("read");
    let legacy = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(
            minisign::sign(None, &p.pair.sk, &bytes[..], None, None)
                .expect("sign")
                .into_string(),
        )
    };
    p.manifest
        .platforms
        .get_mut(PLATFORM)
        .expect("platform")
        .signature = legacy.clone();
    p.manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(PLATFORM)
        .expect("platform")
        .signature = legacy;

    must_refuse(&p, "v1.0.1", "no authenticated release identity");
}

#[test]
fn a_plain_http_url_is_refused_for_a_public_release() {
    // The client refuses these anyway (docs/DECISIONS.md #19), so publishing one
    // produces a release that is both unsafe and unusable -- and the failure
    // lands on users rather than on CI.
    let mut p = publishable();
    p.manifest
        .platforms
        .get_mut(PLATFORM)
        .expect("platform")
        .url = "http://releases.example.com/app_1.0.1.bin".to_owned();

    must_refuse(&p, "v1.0.1", "plain HTTP");
}

#[test]
fn a_plain_http_patch_url_is_refused_too() {
    // The full-download URL is the obvious one to check and the patch URLs are
    // the ones that get forgotten. A client fetches both.
    let mut p = publishable();
    let entry = p
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(PLATFORM)
        .expect("platform");
    entry.patches.get_mut("1.0.0").expect("patch").patch_url =
        "http://releases.example.com/p.zst".to_owned();

    must_refuse(&p, "v1.0.1", "plain HTTP");
}

#[test]
fn a_missing_platform_entry_is_refused() {
    // Publishing patches with nothing for Tauri to download is a release no
    // stock updater can use.
    let mut p = publishable();
    p.manifest.platforms.remove(PLATFORM);
    must_refuse(&p, "v1.0.1", "no matching full-download target");
}

#[test]
fn the_loopback_opt_in_permits_http_and_nothing_else() {
    // The rehearsal harness serves over loopback, so the escape hatch has to
    // exist. It must be the only thing it relaxes.
    let mut p = publishable();
    p.manifest
        .platforms
        .get_mut(PLATFORM)
        .expect("platform")
        .url = "http://127.0.0.1:8080/app_1.0.1.bin".to_owned();

    verify_release(
        &p.manifest,
        &ReleaseUnderTest {
            tag: "v1.0.1",
            app_id: APP_ID,
            platform: PLATFORM,
            artifact: &p.artifact,
            pubkey: &public_key_base64(&p.pair),
            allow_insecure_urls: true,
        },
    )
    .expect("the opt-in permits loopback http");

    // ...and still refuses a wrong tag, which the opt-in has nothing to do with.
    let err = verify_release(
        &p.manifest,
        &ReleaseUnderTest {
            tag: "v2.0.0",
            app_id: APP_ID,
            platform: PLATFORM,
            artifact: &p.artifact,
            pubkey: &public_key_base64(&p.pair),
            allow_insecure_urls: true,
        },
    )
    .expect_err("the opt-in is not a bypass");
    assert!(err.to_string().contains("the tag being published"));
}

#[test]
fn a_release_signed_for_a_different_application_is_refused() {
    // Gate P1's app binding, enforced at publish time. One key, two products,
    // and the wrong artifact staged for upload.
    let p = publishable();
    let err = verify_release(
        &p.manifest,
        &ReleaseUnderTest {
            tag: "v1.0.1",
            app_id: "dev.example.a-different-product",
            platform: PLATFORM,
            artifact: &p.artifact,
            pubkey: &public_key_base64(&p.pair),
            allow_insecure_urls: false,
        },
    )
    .expect_err("an artifact signed for another app must not be published as this one");
    assert!(
        err.to_string().contains("contradicts the release"),
        "got: {err}"
    );
}

#[test]
fn a_release_signed_for_a_different_platform_is_refused() {
    let p = publishable();
    let err = verify_release(
        &p.manifest,
        &ReleaseUnderTest {
            tag: "v1.0.1",
            app_id: APP_ID,
            platform: "darwin-aarch64",
            artifact: &p.artifact,
            pubkey: &public_key_base64(&p.pair),
            allow_insecure_urls: false,
        },
    )
    .expect_err("a platform mismatch must not be published");
    // No darwin entry exists, so this is caught as a missing entry -- which is
    // the same refusal for the same reason.
    assert!(
        err.to_string().contains("refusing to publish"),
        "got: {err}"
    );
}

#[test]
fn a_patch_that_does_not_reconstruct_its_target_is_refused() {
    // Blocker B7, driven directly, because the honest path cannot produce a
    // broken patch on demand -- a test that only asserts "the good case still
    // works" is exactly the test that would not notice this guard being deleted.
    //
    // The patch here is real and correct; it is simply between the wrong pair.
    // That is what a mismatched predecessor looks like: a flaky download, a
    // wrong tag resolved, an asset replaced in place. Nothing about the patch
    // *file* is detectably wrong, which is why hashing it proves nothing and
    // applying it proves everything.
    use tauri_updater_delta_core::backend::{PatchBackend, ZstdBackend};

    let dir = tempfile::tempdir().expect("temp dir");
    let (old, new) = artifacts(dir.path());
    let third = dir.path().join("app_2.0.0.bin");
    std::fs::write(&third, b"a release neither of the others patches into").expect("write");

    let patch = dir.path().join("p.zst");
    ZstdBackend::new()
        .diff(&old, &new, &patch)
        .expect("a real patch, between the wrong pair");

    // Sanity: it does reconstruct what it was actually made for.
    prove_patch_reconstructs(
        &old,
        &patch,
        tauri_updater_delta_core::FileHash::of_file(&new).expect("hash"),
    )
    .expect("the fixture must be a working patch, or this test proves nothing");

    let err = prove_patch_reconstructs(
        &old,
        &patch,
        tauri_updater_delta_core::FileHash::of_file(&third).expect("hash"),
    )
    .expect_err("a patch that reconstructs the wrong artifact must be refused");

    assert!(
        err.to_string().contains("does not reconstruct the release"),
        "got: {err}"
    );
}

#[test]
fn a_corrupted_patch_is_refused_before_it_is_described() {
    // The other shape: the patch file itself is damaged after generation. A
    // digest recorded over the damaged bytes would be perfectly self-consistent.
    use tauri_updater_delta_core::backend::{PatchBackend, ZstdBackend};

    let dir = tempfile::tempdir().expect("temp dir");
    let (old, new) = artifacts(dir.path());
    let patch = dir.path().join("p.zst");
    ZstdBackend::new().diff(&old, &new, &patch).expect("diff");

    let mut bytes = std::fs::read(&patch).expect("read patch");
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&patch, &bytes).expect("corrupt the patch");

    let err = prove_patch_reconstructs(
        &old,
        &patch,
        tauri_updater_delta_core::FileHash::of_file(&new).expect("hash"),
    )
    .expect_err("a corrupted patch must be refused");
    assert!(
        err.to_string().contains("refus") || err.to_string().contains("does not reconstruct"),
        "got: {err}"
    );
}
