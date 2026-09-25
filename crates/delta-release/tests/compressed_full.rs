//! The compressed full copy, from the release tool to the client's rebuild.
//!
//! `add_compressed_full` publishes a zstd copy of the installer as a patch from
//! an empty base; `fetch_compressed_full` downloads it and must rebuild the
//! exact published installer or fail so the caller downloads it uncompressed.
//! See docs/DECISIONS.md #40.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use minisign::KeyPair;
use tauri_updater_delta_core::client::Fetch;
use tauri_updater_delta_core::manifest::Manifest;
use tauri_updater_delta_core::{
    compressed_full_for, fetch_compressed_full, FileHash, Limits, UpdateIdentity,
};
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{add_compressed_full, build_release, ReleaseRequest};

const PLATFORM: &str = "windows-x86_64";
const INSTALLER_URL: &str = "https://releases.example.com/app-setup.exe";
const FULL_URL: &str = "https://releases.example.com/app-setup.exe.zst";

struct Server {
    files: HashMap<String, Vec<u8>>,
    requested: RefCell<Vec<String>>,
}

impl Fetch for Server {
    fn fetch(&self, url: &str, out: &Path) -> Result<(), String> {
        self.requested.borrow_mut().push(url.to_owned());
        let body = self.files.get(url).ok_or_else(|| format!("404 {url}"))?;
        std::fs::write(out, body).map_err(|e| e.to_string())
    }
}

fn keypair() -> KeyPair {
    KeyPair::generate_encrypted_keypair(Some(String::new())).expect("generate keypair")
}

fn signing_key(pair: &KeyPair) -> SigningKey {
    SigningKey::from_str(&pair.sk.to_box(None).expect("box key").into_string(), None)
        .expect("load key")
}

/// Shaped like an uncompressed NSIS installer: mostly compressible content.
fn compressible_installer(dir: &Path) -> std::path::PathBuf {
    let mut bytes = Vec::new();
    for i in 0..40_000u32 {
        bytes.extend_from_slice(format!("resource {i:05}: {}\n", "payload ".repeat(3)).as_bytes());
    }
    let path = dir.join("app-setup.exe");
    std::fs::write(&path, bytes).expect("write installer");
    path
}

fn release(dir: &Path, installer: &Path, pair: &KeyPair) -> Manifest {
    build_release(
        &ReleaseRequest {
            platform: PLATFORM,
            version: "1.0.1",
            new_installer: installer,
            installer_url: INSTALLER_URL,
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            predecessors: &[],
            allow_insecure_urls: false,
        },
        &signing_key(pair),
        None,
    )
    .map(|(manifest, _)| manifest)
    .unwrap_or_else(|e| panic!("release in {}: {e}", dir.display()))
}

fn identity(manifest: &Manifest) -> UpdateIdentity {
    let tauri = &manifest.platforms[PLATFORM];
    UpdateIdentity::new(
        "1.0.0",
        "1.0.1",
        "windows",
        &tauri.url,
        &tauri.signature,
        manifest.to_json().expect("serialise"),
    )
}

#[test]
fn a_compressed_copy_is_published_and_rebuilds_the_exact_installer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let installer = compressible_installer(dir.path());
    let mut manifest = release(dir.path(), &installer, &pair);
    let out = dir.path().join("app-setup.exe.zst");

    let full = add_compressed_full(&mut manifest, PLATFORM, &installer, FULL_URL, &out, false)
        .expect("compress")
        .expect("a compressible installer must get a compressed copy");
    let installer_size = std::fs::metadata(&installer).expect("stat").len();
    assert!(
        full.size < installer_size / 4,
        "{} bytes is not a useful compression of {installer_size}",
        full.size
    );

    // Survives serialisation, and the client finds it through Tauri's selection.
    let reparsed = Manifest::from_json(&manifest.to_json().expect("json")).expect("parse");
    let (found, target) = compressed_full_for(&identity(&reparsed)).expect("published");
    assert_eq!(found, full);

    let server = Server {
        files: HashMap::from([(FULL_URL.to_owned(), std::fs::read(&out).expect("read"))]),
        requested: RefCell::new(Vec::new()),
    };
    let work = dir.path().join("work");
    let rebuilt =
        fetch_compressed_full(&found, &target, Limits::default(), &work, &server).expect("rebuild");
    assert_eq!(
        FileHash::of_file(&rebuilt).expect("hash"),
        FileHash::of_file(&installer).expect("hash"),
        "the rebuilt installer must be byte-identical to the published one"
    );
    assert_eq!(*server.requested.borrow(), vec![FULL_URL.to_owned()]);
}

#[test]
fn a_tampered_compressed_copy_is_rejected_before_decoding() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let installer = compressible_installer(dir.path());
    let mut manifest = release(dir.path(), &installer, &pair);
    let out = dir.path().join("app-setup.exe.zst");
    add_compressed_full(&mut manifest, PLATFORM, &installer, FULL_URL, &out, false)
        .expect("compress")
        .expect("published");
    let (found, target) = compressed_full_for(&identity(&manifest)).expect("published");

    let mut body = std::fs::read(&out).expect("read");
    let middle = body.len() / 2;
    body[middle] ^= 0xff;
    let server = Server {
        files: HashMap::from([(FULL_URL.to_owned(), body)]),
        requested: RefCell::new(Vec::new()),
    };

    let err = fetch_compressed_full(&found, &target, Limits::default(), dir.path(), &server)
        .expect_err("a corrupted copy must not rebuild anything");
    assert!(
        err.to_string().contains("checksum") || err.to_string().contains("mismatch"),
        "got: {err}"
    );
}

#[test]
fn an_incompressible_installer_gets_no_compressed_copy() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let installer = dir.path().join("app-setup.exe");
    // xorshift64: statistically random enough that zstd cannot shrink it.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let noise: Vec<u8> = (0..200_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect();
    std::fs::write(&installer, noise).expect("write");
    let mut manifest = release(dir.path(), &installer, &pair);
    let out = dir.path().join("app-setup.exe.zst");

    let published = add_compressed_full(&mut manifest, PLATFORM, &installer, FULL_URL, &out, false)
        .expect("compress");

    assert_eq!(published, None);
    assert!(
        !out.exists(),
        "an unpublished copy must not be left for an upload glob"
    );
    assert!(compressed_full_for(&identity(&manifest)).is_none());
}

#[test]
fn a_different_installer_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let installer = compressible_installer(dir.path());
    let mut manifest = release(dir.path(), &installer, &pair);
    let other = dir.path().join("other.exe");
    std::fs::write(&other, b"not the release").expect("write");

    let err = add_compressed_full(
        &mut manifest,
        PLATFORM,
        &other,
        FULL_URL,
        &dir.path().join("x.zst"),
        false,
    )
    .expect_err("only the described installer may be compressed");
    assert!(err.to_string().contains("not the installer"), "got: {err}");
}
