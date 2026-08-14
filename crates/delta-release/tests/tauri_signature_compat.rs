//! Gate: does the *actual* Tauri updater accept what `delta-release` signs?
//!
//! Everything in this project rests on one assumption — that the signature we
//! write into a manifest is the signature Tauri's official updater will check
//! and accept. If that is wrong, the delta path installs artifacts Tauri would
//! have rejected, and every layer built on top has to be reworked. So it is
//! settled here, before any client install flow exists.
//!
//! "`minisign-verify` accepts it" and "the Tauri updater accepts it" are
//! different claims. This file tests the second one by reproducing
//! `tauri-plugin-updater`'s verifier **verbatim** rather than writing an
//! equivalent-looking check of our own.
//!
//! Reproduced from `tauri-plugin-updater` 2.10.1, `src/updater.rs:1453`:
//!
//! ```ignore
//! fn verify_signature(data: &[u8], release_signature: &str, pub_key: &str) -> Result<bool> {
//!     let pub_key_decoded = base64_to_string(pub_key)?;
//!     let public_key = PublicKey::decode(&pub_key_decoded)?;
//!     let signature_base64_decoded = base64_to_string(release_signature)?;
//!     let signature = Signature::decode(&signature_base64_decoded)?;
//!     public_key.verify(data, &signature, true)?;
//!     Ok(true)
//! }
//!
//! fn base64_to_string(base64_string: &str) -> Result<String> {
//!     let decoded_string = &base64::engine::general_purpose::STANDARD.decode(base64_string)?;
//!     let result = std::str::from_utf8(decoded_string)?.to_string();
//!     Ok(result)
//! }
//! ```
//!
//! Note `PublicKey::decode` on the whole key file, not `from_base64` on the key
//! line — a check written from memory would plausibly get that wrong and still
//! pass, which is exactly why this is copied rather than paraphrased.
//!
//! # What this proves, and what it does not
//!
//! Proven here: the bytes `delta-release` produces satisfy Tauri's verification
//! algorithm, using the same `minisign-verify` version Tauri depends on
//! (`0.2`), through the same call sequence, including the prehashed code path.
//!
//! Not proven here: that a *running* Tauri application accepts an update served
//! this way. That needs the example app, and remains the final confirmation.
//! This gate exists so that if the algorithm is wrong, we find out now rather
//! than after building a client on top of it.

use std::path::Path;

use base64::Engine as _;
use minisign::KeyPair;
use minisign_verify::{PublicKey, Signature};
use tauri_updater_delta_core::manifest::Manifest;
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{build_release, Predecessor, ReleaseRequest};

const PLATFORM: &str = "linux-x86_64";

/// Verbatim reproduction of `tauri-plugin-updater`'s `base64_to_string`.
fn base64_to_string(base64_string: &str) -> std::result::Result<String, String> {
    let decoded_string = base64::engine::general_purpose::STANDARD
        .decode(base64_string)
        .map_err(|e| e.to_string())?;
    let result = std::str::from_utf8(&decoded_string)
        .map_err(|e| e.to_string())?
        .to_string();
    Ok(result)
}

/// Verbatim reproduction of `tauri-plugin-updater`'s `verify_signature`.
///
/// Deliberately not refactored, not tidied, and not made to return a nicer
/// error type. Its value is in being the same code.
fn tauri_verify_signature(
    data: &[u8],
    release_signature: &str,
    pub_key: &str,
) -> std::result::Result<bool, String> {
    let pub_key_decoded = base64_to_string(pub_key)?;
    let public_key = PublicKey::decode(&pub_key_decoded).map_err(|e| e.to_string())?;
    let signature_base64_decoded = base64_to_string(release_signature)?;
    let signature = Signature::decode(&signature_base64_decoded).map_err(|e| e.to_string())?;

    public_key
        .verify(data, &signature, true)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

fn keypair() -> KeyPair {
    KeyPair::generate_encrypted_keypair(Some(String::new())).expect("generate keypair")
}

fn signing_key(pair: &KeyPair) -> SigningKey {
    SigningKey::from_str(&pair.sk.to_box(None).expect("box key").into_string(), None)
        .expect("load key")
}

/// The public key exactly as it is stored in `tauri.conf.json`: base64 of the
/// whole `.pub` file, comment line included.
fn tauri_pubkey(pair: &KeyPair) -> String {
    let file = pair.pk.to_box().expect("box public key").into_string();
    base64::engine::general_purpose::STANDARD.encode(file)
}

/// Run the real release path and return (manifest, released installer path).
fn publish(dir: &Path, pair: &KeyPair) -> (Manifest, std::path::PathBuf) {
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir);
    let patch = dir.join("patch.zst");

    let (manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &fixture.new,
            installer_url: "https://example.com/app_1.0.1.AppImage",
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            predecessor: Some(Predecessor {
                from_version: "1.0.0",
                installer: &fixture.old,
                patch_url: "https://example.com/patch.zst",
                patch_out: &patch,
                tar_layer: None,
            }),
        },
        &signing_key(pair),
        None,
    )
    .expect("release should build");

    (manifest, fixture.new)
}

#[test]
fn tauri_accepts_the_signature_in_the_full_download_layer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let (manifest, installer) = publish(dir.path(), &pair);

    let bytes = std::fs::read(&installer).expect("read installer");
    let signature = &manifest.platforms[PLATFORM].signature;

    let verified = tauri_verify_signature(&bytes, signature, &tauri_pubkey(&pair))
        .expect("Tauri's own verifier must accept the signature we publish");
    assert!(verified);
}

#[test]
fn tauri_accepts_the_signature_in_the_delta_layer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let (manifest, installer) = publish(dir.path(), &pair);

    let bytes = std::fs::read(&installer).expect("read installer");
    let signature = &manifest.delta.as_ref().expect("delta layer").platforms[PLATFORM].signature;

    let verified = tauri_verify_signature(&bytes, signature, &tauri_pubkey(&pair))
        .expect("the delta layer's signature must satisfy Tauri too");
    assert!(verified);
}

#[test]
fn the_signature_is_prehashed_and_that_is_the_path_tauri_takes() {
    // The specific question: Tauri passes `allow_legacy = true`, so a *legacy*
    // signature would also be accepted — meaning a passing verification alone
    // does not tell us which code path ran. `minisign` signs with
    // SIGALG_PREHASHED, and minisign-verify's `verify` branches on
    // `signature.is_prehashed` before `allow_legacy` is even consulted. Assert
    // the flag directly so we know the prehashed branch is the one being
    // exercised, rather than inferring it from a green test.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let (manifest, installer) = publish(dir.path(), &pair);
    let bytes = std::fs::read(&installer).expect("read installer");

    let raw = base64_to_string(&manifest.platforms[PLATFORM].signature).expect("base64 decode");
    let signature = Signature::decode(&raw).expect("decode signature");

    // On the wire: minisign puts the algorithm in the first two bytes of the
    // signature line. "Ed" (0x45 0x64) is legacy, "ED" (0x45 0x44) is prehashed.
    let algorithm = raw
        .lines()
        .nth(1)
        .map(|line| {
            base64::engine::general_purpose::STANDARD
                .decode(line)
                .expect("signature line is base64")
        })
        .expect("signature has an algorithm line");
    assert_eq!(
        (algorithm[0], algorithm[1]),
        (0x45, 0x44),
        "delta-release must emit prehashed (ED) signatures, got {:?}",
        &algorithm[..2]
    );

    // And through the public API, without relying on the wire format: verifying
    // with `allow_legacy = false` rejects any non-prehashed signature outright,
    // so success here can only mean the prehashed branch ran.
    let public_key =
        PublicKey::decode(&base64_to_string(&tauri_pubkey(&pair)).expect("decode key"))
            .expect("public key");
    public_key
        .verify(&bytes, &signature, false)
        .expect("signature must verify even with legacy signatures disallowed");
}

#[test]
fn tauri_rejects_a_signature_over_different_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let (manifest, _) = publish(dir.path(), &pair);

    let result = tauri_verify_signature(
        b"an artifact that was never signed",
        &manifest.platforms[PLATFORM].signature,
        &tauri_pubkey(&pair),
    );
    assert!(
        result.is_err(),
        "Tauri must reject a signature that does not cover the bytes"
    );
}

#[test]
fn tauri_rejects_a_signature_from_another_key() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let (manifest, installer) = publish(dir.path(), &pair);
    let bytes = std::fs::read(&installer).expect("read installer");

    let result = tauri_verify_signature(
        &bytes,
        &manifest.platforms[PLATFORM].signature,
        &tauri_pubkey(&keypair()),
    );
    assert!(
        result.is_err(),
        "Tauri must reject a signature made by a key it does not trust"
    );
}

#[test]
fn a_rebuilt_artifact_satisfies_the_same_signature() {
    // The point of signing the target installer rather than the patch: a user
    // who reconstructed the artifact from a delta holds bytes that satisfy the
    // very same signature a full download would have been checked against.
    use tauri_updater_delta_core::{try_reconstruct, Reconstruction, TargetSpec};

    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let fixture = tauri_updater_delta_fixtures::appimage_pair(dir.path());
    let patch = dir.path().join("patch.zst");

    let (manifest, _) = build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: &fixture.new,
            installer_url: "https://example.com/app_1.0.1.AppImage",
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            predecessor: Some(Predecessor {
                from_version: "1.0.0",
                installer: &fixture.old,
                patch_url: "https://example.com/patch.zst",
                patch_out: &patch,
                tar_layer: None,
            }),
        },
        &signing_key(&pair),
        None,
    )
    .expect("release should build");

    let entry = &manifest.delta.as_ref().expect("delta layer").platforms[PLATFORM];
    let target = TargetSpec::new(
        "zstd",
        entry.target_installer_size,
        &entry.target_installer_blake3,
    )
    .expect("target spec");

    let rebuilt = dir.path().join("rebuilt.AppImage");
    let Reconstruction::Verified(path) = try_reconstruct(&fixture.old, &patch, &rebuilt, &target)
    else {
        panic!("reconstruction should succeed");
    };

    let bytes = std::fs::read(&path).expect("read rebuilt artifact");
    let verified = tauri_verify_signature(
        &bytes,
        &manifest.platforms[PLATFORM].signature,
        &tauri_pubkey(&pair),
    )
    .expect("a delta-rebuilt artifact must satisfy Tauri's signature check");
    assert!(verified);
}
