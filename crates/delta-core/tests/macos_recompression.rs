//! The recompression recipe, checked against a real published artifact.
//!
//! The unit tests in `src/recompress.rs` build their expected output with the
//! `tar::Builder` this build links against. That proves the topology is
//! self-consistent, and it cannot prove more than that: if this crate's idea of
//! the write pattern were wrong in the same way the reference construction is,
//! both would agree and both would be wrong.
//!
//! This file removes that circularity. The fixture is an artifact
//! `cargo tauri build` actually produced — tauri-cli 2.10.1, tauri-bundler
//! 2.8.1, on 2026-08-13 — retained byte-for-byte since. Nothing in this
//! repository produced it, and nothing in this repository can adjust it to
//! agree.
//!
//! Two claims are asserted, and they are different claims:
//!
//! 1. **Byte identity.** Decompress the fixture, recompress the tar with the
//!    recipe, get the fixture back.
//! 2. **Signature acceptance.** The minisign signature issued over the original
//!    artifact verifies against the *rebuilt* one — which is the property the
//!    whole tar layer rests on, because `Update::install` consumes the
//!    compressed bytes and Tauri does not check them (`docs/DECISIONS.md` #10).
//!
//! (1) implies (2) mathematically. They are both here because a failure of (2)
//! alone would mean the verification path had changed, and that is worth being
//! told about separately.

use std::path::PathBuf;

use tauri_updater_delta_core::recompress::{decompress_bounded, recompress_app_tar_gz};
use tauri_updater_delta_core::{verify_artifact, FileHash};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/macos-app-tar-gz")
}

struct Fixture {
    official: Vec<u8>,
    signature: String,
    pubkey: String,
    declared_blake3: String,
    declared_size: u64,
}

fn fixture() -> Fixture {
    let dir = fixtures();
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("official-1.0.1.json")).expect(
            "the controlled fixture metadata must be present; see tests/fixtures/README.md",
        ),
    )
    .expect("fixture metadata is JSON");

    let artifact = dir.join(meta["artifact"].as_str().expect("artifact name"));
    Fixture {
        official: std::fs::read(&artifact)
            .expect("the controlled fixture artifact must be present"),
        signature: meta["signature"].as_str().expect("signature").to_owned(),
        pubkey: meta["pubkey"].as_str().expect("pubkey").to_owned(),
        declared_blake3: meta["installer_blake3"]
            .as_str()
            .expect("digest")
            .to_owned(),
        declared_size: meta["installer_size"].as_u64().expect("size"),
    }
}

/// Guards the fixture itself. If this fails, every other assertion here is
/// about the wrong bytes.
#[test]
fn the_fixture_is_the_artifact_the_experiment_recorded() {
    let f = fixture();
    assert_eq!(f.official.len() as u64, f.declared_size);
    assert_eq!(FileHash::of_bytes(&f.official).to_hex(), f.declared_blake3);
    assert_eq!(
        f.declared_blake3, "718c104bfdcc00aaa7b588519ad62c2719a182d8e7364c1daf260ed8fbf96f36",
        "the digest recorded by research/experiments/2026-08-13-macos-controlled-tar-layer"
    );
    verify_artifact(f.official.clone(), &f.signature, &f.pubkey)
        .expect("the fixture's own signature must verify, or the fixture is wrong");
}

#[test]
fn rebuilds_a_real_published_app_tar_gz_byte_for_byte() {
    let dir = tempfile::tempdir().expect("temp dir");
    let f = fixture();

    let artifact = dir.path().join("official.app.tar.gz");
    std::fs::write(&artifact, &f.official).expect("write fixture");

    let tar = dir.path().join("exact.tar");
    let tar_size = decompress_bounded(&artifact, &tar, 64 * 1024 * 1024).expect("decompress");
    assert_eq!(
        tar_size, 9_461_248,
        "the tar inside the controlled artifact, as recorded by the tar-layer experiment"
    );

    let rebuilt_path = dir.path().join("rebuilt.app.tar.gz");
    recompress_app_tar_gz(&tar, &rebuilt_path).expect("recompress");
    let rebuilt = std::fs::read(&rebuilt_path).expect("read rebuilt");

    assert_eq!(
        rebuilt.len(),
        f.official.len(),
        "rebuilt artifact is a different length from the published one"
    );
    let first_difference = rebuilt
        .iter()
        .zip(f.official.iter())
        .position(|(a, b)| a != b);
    assert_eq!(
        first_difference, None,
        "rebuilt artifact diverges from the published one"
    );
    assert_eq!(
        FileHash::of_bytes(&rebuilt).to_hex(),
        f.declared_blake3,
        "the rebuilt artifact must hash to what the release published"
    );
}

#[test]
fn the_published_signature_accepts_the_rebuilt_artifact() {
    // The load-bearing consequence. If this ever fails while the byte-identity
    // test passes, verification has changed, not compression.
    let dir = tempfile::tempdir().expect("temp dir");
    let f = fixture();

    let artifact = dir.path().join("official.app.tar.gz");
    std::fs::write(&artifact, &f.official).expect("write fixture");
    let tar = dir.path().join("exact.tar");
    decompress_bounded(&artifact, &tar, 64 * 1024 * 1024).expect("decompress");
    let rebuilt_path = dir.path().join("rebuilt.app.tar.gz");
    recompress_app_tar_gz(&tar, &rebuilt_path).expect("recompress");
    let rebuilt = std::fs::read(&rebuilt_path).expect("read rebuilt");

    let verified = verify_artifact(rebuilt, &f.signature, &f.pubkey)
        .expect("the signature over the published artifact must accept the rebuilt one");
    assert_eq!(verified.as_bytes(), &f.official[..]);
}

#[test]
fn a_decompression_ceiling_below_the_tar_refuses_it() {
    // The cache path decompresses artifacts it does not trust, so the ceiling
    // has to bite before the disk does.
    let dir = tempfile::tempdir().expect("temp dir");
    let f = fixture();
    let artifact = dir.path().join("official.app.tar.gz");
    std::fs::write(&artifact, &f.official).expect("write fixture");

    let out = dir.path().join("bounded.tar");
    let err = decompress_bounded(&artifact, &out, 1024).expect_err("must refuse");
    assert!(
        matches!(
            err,
            tauri_updater_delta_core::Error::OutputTooLarge { limit: 1024 }
        ),
        "unexpected error: {err}"
    );
    assert!(
        !out.exists(),
        "a refused decompression must not leave a truncated tar behind"
    );
}
