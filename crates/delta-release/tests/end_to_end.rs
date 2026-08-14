//! The full loop: release tooling produces a manifest, a client consumes it.
//!
//! Phase 1 proved a patch can rebuild an artifact. Phase 2 has to prove the two
//! halves actually fit together — that what the release tool writes is exactly
//! what a client needs, with nothing assumed and nothing passed out of band.
//!
//! So these tests never construct a manifest by hand. They run the real release
//! path, serialise the result to JSON, throw the in-memory value away, and give
//! the client nothing but that JSON and the files it names. Anything the tool
//! forgets to record shows up here as a failure rather than as a surprise in
//! Phase 3.
//!
//! The second half is the safety model: corrupt, truncated and wrong-base
//! patches must each produce a fall back to the full download, never an install.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use minisign::KeyPair;
use tauri_updater_delta_core::manifest::Manifest;
use tauri_updater_delta_core::{try_reconstruct, FileHash, Reconstruction, TargetSpec};
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{build_release, Predecessor, ReleaseRequest};

const PLATFORM: &str = "linux-x86_64";

/// A keypair that can be serialised and read back.
///
/// `generate_unencrypted_keypair()` leaves the secret key checksum zeroed and
/// its output cannot be loaded again; an empty password writes the checksum and
/// skips encryption, which is what an unencrypted key really looks like.
fn keypair() -> KeyPair {
    KeyPair::generate_encrypted_keypair(Some(String::new())).expect("generate keypair")
}

fn signing_key(pair: &KeyPair) -> SigningKey {
    let text = pair.sk.to_box(None).expect("box key").into_string();
    SigningKey::from_str(&text, None).expect("load key")
}

fn public_key_base64(pair: &KeyPair) -> String {
    pair.pk
        .to_box()
        .expect("box public key")
        .into_string()
        .lines()
        .nth(1)
        .expect("public key line")
        .to_owned()
}

/// What a release run leaves on disk, plus the JSON a client would fetch.
struct Published {
    manifest_json: String,
    patch: PathBuf,
    installed: PathBuf,
    released: PathBuf,
}

/// Run the real release path over the shared AppImage fixtures.
fn publish(dir: &Path, pair: &KeyPair) -> Published {
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir);
    let patch = dir.join("1.0.0-to-1.0.1.zst");

    let request = ReleaseRequest {
        platform: PLATFORM,
        version: tauri_updater_delta_fixtures::NEW_VERSION,
        new_installer: &fixture.new,
        installer_url: "https://releases.example.com/app_1.0.1.AppImage",
        notes: Some("Phase 2 end-to-end"),
        pub_date: Some("2026-08-12T10:00:00Z"),
        app_id: "dev.example.testapp",
        predecessor: Some(Predecessor {
            from_version: tauri_updater_delta_fixtures::OLD_VERSION,
            installer: &fixture.old,
            patch_url: "https://releases.example.com/1.0.0-to-1.0.1.zst",
            patch_out: &patch,
            tar_layer: None,
        }),
        allow_insecure_urls: false,
    };

    let (manifest, _) =
        build_release(&request, &signing_key(pair), None).expect("release should build");

    Published {
        manifest_json: manifest.to_json().expect("serialise manifest"),
        patch,
        installed: fixture.old,
        released: fixture.new,
    }
}

/// Verify a signature the way Tauri's updater does.
fn signature_is_valid(artifact: &Path, signature_b64: &str, pair: &KeyPair) -> bool {
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(signature_b64) else {
        return false;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return false;
    };
    let Ok(signature) = minisign_verify::Signature::decode(&text) else {
        return false;
    };
    let Ok(public) = minisign_verify::PublicKey::from_base64(&public_key_base64(pair)) else {
        return false;
    };
    let Ok(bytes) = std::fs::read(artifact) else {
        return false;
    };
    public.verify(&bytes, &signature, true).is_ok()
}

/// What a client concluded after trying the delta path.
#[derive(Debug, PartialEq, Eq)]
enum ClientOutcome {
    /// Rebuilt, hash-verified and signature-verified. Safe to install.
    Installed,
    /// Something was wrong. Download the full artifact instead.
    FullDownload,
}

/// Simulate the client half of an update.
///
/// Deliberately given only the manifest JSON, the base artifact and a patch
/// file standing in for a download — the same inputs Phase 3's plugin will have.
fn run_client(
    manifest_json: &str,
    base: &Path,
    downloaded_patch: &Path,
    out: &Path,
    pair: &KeyPair,
) -> ClientOutcome {
    let Ok(manifest) = Manifest::from_json(manifest_json) else {
        return ClientOutcome::FullDownload;
    };

    let Some((entry, patch)) =
        manifest.patch_for(PLATFORM, tauri_updater_delta_fixtures::OLD_VERSION)
    else {
        return ClientOutcome::FullDownload;
    };

    // Check the patch against the manifest before spending anything applying it.
    match FileHash::of_file(downloaded_patch) {
        Ok(actual) if actual.to_hex() == patch.patch_blake3 => {}
        _ => return ClientOutcome::FullDownload,
    }

    let Ok(target) = TargetSpec::new(
        &patch.backend_id,
        entry.target_installer_size,
        &entry.target_installer_blake3,
    ) else {
        return ClientOutcome::FullDownload;
    };

    match try_reconstruct(base, downloaded_patch, out, &target) {
        Reconstruction::FallBack(_) => ClientOutcome::FullDownload,
        Reconstruction::Verified(path) => {
            // The hash proved reconstruction; the signature proves provenance.
            // Tauri's updater would do this check itself against the same field.
            if signature_is_valid(&path, &entry.signature, pair) {
                ClientOutcome::Installed
            } else {
                ClientOutcome::FullDownload
            }
        }
    }
}

#[test]
fn a_client_updates_from_nothing_but_the_manifest() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let published = publish(dir.path(), &pair);
    let out = dir.path().join("rebuilt.AppImage");

    let outcome = run_client(
        &published.manifest_json,
        &published.installed,
        &published.patch,
        &out,
        &pair,
    );

    assert_eq!(
        outcome,
        ClientOutcome::Installed,
        "the manifest the tool wrote must be sufficient on its own"
    );

    // And what it installed is the released artifact, byte for byte.
    let expected = FileHash::of_file(&published.released).expect("hash released artifact");
    let actual = FileHash::of_file(&out).expect("hash rebuilt artifact");
    assert_eq!(actual, expected, "the client installed the wrong bytes");
}

#[test]
fn a_corrupt_patch_falls_back_instead_of_installing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let published = publish(dir.path(), &pair);
    let out = dir.path().join("rebuilt.AppImage");

    let mut bytes = std::fs::read(&published.patch).expect("read patch");
    let middle = bytes.len() / 2;
    for byte in &mut bytes[middle..middle + 128] {
        *byte ^= 0xA5;
    }
    let corrupt = dir.path().join("corrupt.zst");
    std::fs::write(&corrupt, &bytes).expect("write corrupt patch");

    assert_eq!(
        run_client(
            &published.manifest_json,
            &published.installed,
            &corrupt,
            &out,
            &pair
        ),
        ClientOutcome::FullDownload
    );
    assert!(
        !out.exists(),
        "nothing should be left for an installer to pick up"
    );
}

#[test]
fn a_truncated_patch_falls_back_instead_of_installing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let published = publish(dir.path(), &pair);
    let out = dir.path().join("rebuilt.AppImage");

    let bytes = std::fs::read(&published.patch).expect("read patch");
    let cut = dir.path().join("cut.zst");
    std::fs::write(&cut, &bytes[..bytes.len() * 2 / 3]).expect("write truncated patch");

    assert_eq!(
        run_client(
            &published.manifest_json,
            &published.installed,
            &cut,
            &out,
            &pair
        ),
        ClientOutcome::FullDownload
    );
    assert!(
        !out.exists(),
        "nothing should be left for an installer to pick up"
    );
}

#[test]
fn the_wrong_base_version_falls_back_instead_of_installing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let published = publish(dir.path(), &pair);
    let out = dir.path().join("rebuilt.AppImage");

    // A user whose installed artifact is not the one the patch was built against.
    let wrong = dir.path().join("wrong_base.AppImage");
    std::fs::write(&wrong, b"some other version entirely").expect("write wrong base");

    assert_eq!(
        run_client(
            &published.manifest_json,
            &wrong,
            &published.patch,
            &out,
            &pair
        ),
        ClientOutcome::FullDownload
    );
    assert!(
        !out.exists(),
        "nothing should be left for an installer to pick up"
    );
}

#[test]
fn a_manifest_signed_by_another_key_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let published = publish(dir.path(), &keypair());
    let out = dir.path().join("rebuilt.AppImage");

    // Reconstruction will succeed and the hash will match — only the signature
    // is wrong. This is the check that stops a correct-looking artifact from a
    // compromised or mistaken release process.
    let attacker = keypair();
    assert_eq!(
        run_client(
            &published.manifest_json,
            &published.installed,
            &published.patch,
            &out,
            &attacker
        ),
        ClientOutcome::FullDownload,
        "a valid reconstruction under the wrong key must not install"
    );
}

#[test]
fn an_unknown_backend_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let published = publish(dir.path(), &pair);
    let out = dir.path().join("rebuilt.AppImage");

    // A future release tool using a backend this client does not implement.
    let tampered = published
        .manifest_json
        .replace(r#""backend_id": "zstd""#, r#""backend_id": "bsdiff""#);
    assert_ne!(
        tampered, published.manifest_json,
        "the backend id should be present to rewrite"
    );

    assert_eq!(
        run_client(
            &tampered,
            &published.installed,
            &published.patch,
            &out,
            &pair
        ),
        ClientOutcome::FullDownload
    );
}

#[test]
fn a_client_on_an_unlisted_version_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let published = publish(dir.path(), &pair);

    let manifest = Manifest::from_json(&published.manifest_json).expect("parse manifest");
    // Not an error — just no patch path published for this user yet.
    assert!(manifest.patch_for(PLATFORM, "0.1.0").is_none());
    assert!(manifest.patch_for("windows-x86_64", "1.0.0").is_none());
}

#[test]
fn the_published_manifest_is_a_valid_tauri_document() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let published = publish(dir.path(), &pair);

    // What a delta-unaware Tauri client sees, with our layer ignored entirely.
    let value: serde_json::Value =
        serde_json::from_str(&published.manifest_json).expect("valid json");

    assert_eq!(value["version"], "1.0.1");
    assert_eq!(value["notes"], "Phase 2 end-to-end");
    assert_eq!(value["pub_date"], "2026-08-12T10:00:00Z");

    let platform = &value["platforms"][PLATFORM];
    assert_eq!(
        platform["url"],
        "https://releases.example.com/app_1.0.1.AppImage"
    );

    // The signature in the Tauri layer must validate the released installer —
    // this is the fallback path, and it has to work on its own.
    let signature = platform["signature"].as_str().expect("signature present");
    assert!(
        signature_is_valid(&published.released, signature, &pair),
        "the full-download path's own signature must verify the released artifact"
    );
}

#[test]
fn both_layers_carry_the_same_signature() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let published = publish(dir.path(), &pair);
    let manifest = Manifest::from_json(&published.manifest_json).expect("parse manifest");

    let tauri = &manifest.platforms[PLATFORM].signature;
    let delta = &manifest.delta.as_ref().expect("delta layer").platforms[PLATFORM].signature;

    // Both paths install the same artifact, so one signature covers both. If
    // these ever diverge, one of the two paths is validating something else.
    assert_eq!(tauri, delta);
}

#[test]
fn a_second_upgrade_path_joins_the_existing_manifest() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let key = signing_key(&pair);
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir.path());

    // First release run: 1.0.0 -> 1.0.1.
    let first_patch = dir.path().join("from-1.0.0.zst");
    let (manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &fixture.new,
            installer_url: "https://example.com/app.AppImage",
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            predecessor: Some(Predecessor {
                from_version: "1.0.0",
                installer: &fixture.old,
                patch_url: "https://example.com/from-1.0.0.zst",
                patch_out: &first_patch,
                tar_layer: None,
            }),
            allow_insecure_urls: false,
        },
        &key,
        None,
    )
    .expect("first release");

    // Second run for the same release, from a different installed version.
    let older = tauri_updater_delta_fixtures::write(dir.path(), "app_0.9.0.AppImage", b"older");
    let second_patch = dir.path().join("from-0.9.0.zst");
    let (manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &fixture.new,
            installer_url: "https://example.com/app.AppImage",
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            predecessor: Some(Predecessor {
                from_version: "0.9.0",
                installer: &older,
                patch_url: "https://example.com/from-0.9.0.zst",
                patch_out: &second_patch,
                tar_layer: None,
            }),
            allow_insecure_urls: false,
        },
        &key,
        Some(manifest),
    )
    .expect("second release");

    let patches = &manifest.delta.as_ref().expect("delta layer").platforms[PLATFORM].patches;
    assert_eq!(
        patches.len(),
        2,
        "publishing a second upgrade path must not discard the first"
    );
    assert!(patches.contains_key("1.0.0"));
    assert!(patches.contains_key("0.9.0"));
}

#[test]
fn a_new_release_replaces_patches_that_target_the_old_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let key = signing_key(&pair);
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir.path());

    let (old_manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &fixture.new,
            installer_url: "https://example.com/app.AppImage",
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            predecessor: Some(Predecessor {
                from_version: "1.0.0",
                installer: &fixture.old,
                patch_url: "https://example.com/a.zst",
                patch_out: &dir.path().join("a.zst"),
                tar_layer: None,
            }),
            allow_insecure_urls: false,
        },
        &key,
        None,
    )
    .expect("first release");

    // Schema 1 only patches to the latest release, so entries reconstructing
    // 1.0.1 are stale the moment 1.0.2 ships.
    let (new_manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.2",
            new_installer: &fixture.new,
            installer_url: "https://example.com/app.AppImage",
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            predecessor: Some(Predecessor {
                from_version: "1.0.1",
                installer: &fixture.old,
                patch_url: "https://example.com/b.zst",
                patch_out: &dir.path().join("b.zst"),
                tar_layer: None,
            }),
            allow_insecure_urls: false,
        },
        &key,
        Some(old_manifest),
    )
    .expect("second release");

    assert_eq!(new_manifest.version, "1.0.2");
    let patches = &new_manifest.delta.as_ref().expect("delta layer").platforms[PLATFORM].patches;
    assert!(patches.contains_key("1.0.1"));
    assert!(
        !patches.contains_key("1.0.0"),
        "patches rebuilding the previous release must not survive into the next one"
    );
}

#[test]
fn refuses_to_patch_a_version_to_itself() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir.path());

    let result = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &fixture.new,
            installer_url: "https://example.com/app.AppImage",
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            predecessor: Some(Predecessor {
                from_version: "1.0.1",
                installer: &fixture.old,
                patch_url: "https://example.com/a.zst",
                patch_out: &dir.path().join("a.zst"),
                tar_layer: None,
            }),
            allow_insecure_urls: false,
        },
        &signing_key(&pair),
        None,
    );

    assert!(result.is_err(), "a self-patch is never useful");
}
