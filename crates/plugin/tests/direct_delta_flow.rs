//! The managed direct-patch path: a Windows or Linux client's whole delta route.
//!
//! # What was structurally impossible before this file existed
//!
//! The engine could always diff and apply two arbitrary files, and
//! `delta-release` could always generate an `opaque-v1` patch between two
//! Windows installers. None of that reached a real client, because four things
//! were linked:
//!
//! 1. `Update::install_blocking` set the direct-patch base to `None` in every
//!    non-`test-support` build, and nothing else could supply one;
//! 2. `plan_update` consulted the managed cache only for the *tar* path — the
//!    direct path read `ctx.base` and stopped there;
//! 3. `stage_pending` gunzipped every verified artifact to record a tar digest,
//!    so an NSIS `.exe` could not be persisted at all (non-fatally, so the
//!    install succeeded and every later update silently stayed cache-cold); and
//! 4. the cache namespace hard-coded `app-tar-gz-v1` on every operating system.
//!
//! So the expected Windows behaviour was Full, then Full, then Full, for ever.
//!
//! # Why these tests run everywhere
//!
//! The artifacts here are opaque blobs — not gzipped tarballs — and the cache is
//! opened under the `opaque-v1` representation explicitly rather than by asking
//! the platform. That makes this the *Windows* path, exercised on all three CI
//! platforms, which is the only way a macOS-hosted change that breaks it fails
//! before a Windows runner sees it. The real NSIS install evidence is a separate
//! thing and lives in `examples/desktop-app/e2e/`; nothing here claims it.
//!
//! Every fallback case asserts that the direct path was **attempted** before it
//! fell back. A fallback test that passes because the path was never reached
//! proves nothing, which is exactly how a delta updater that never deltaed
//! passed four gates of green tests (`docs/DECISIONS.md` #22).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use minisign::KeyPair;
use tauri_plugin_updater_delta::test_support::{
    run_update, run_update_with_cache_diagnostic, Context, InstallHandoff, Outcome,
};
use tauri_plugin_updater_delta::Error;
use tauri_updater_delta_core::cache::{
    ArtifactCache, CacheLimits, CachedRepresentation, Namespace, Reconciliation, RECOMPRESSION_NONE,
};
use tauri_updater_delta_core::client::{plan_update, Fetch, PlanContext, UpdateSource};
use tauri_updater_delta_core::manifest::{
    Manifest, RECOMPRESSION_TAURI_APP_TAR_GZ_V1, REPRESENTATION_APP_TAR_GZ_V1,
};
use tauri_updater_delta_core::release_identity::{current_platform, REPRESENTATION_OPAQUE_V1};
use tauri_updater_delta_core::{FileHash, Limits, UpdateIdentity, VerifiedArtifact};
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{build_release, Predecessor, ReleaseRequest};

const APP_ID: &str = "dev.example.testapp";

fn installer_url(version: &str) -> String {
    format!("https://example.com/App_{version}_x64-setup.exe")
}

fn patch_url(from: &str, to: &str) -> String {
    format!("https://example.com/{from}-to-{to}.zst")
}

// ---- fakes for the two injectable boundaries ----------------------------

struct FakeServer {
    files: RefCell<HashMap<String, Vec<u8>>>,
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

impl FakeServer {
    fn fetched(&self, url: &str) -> bool {
        self.requested.borrow().iter().any(|u| u == url)
    }
    fn forget(&self) {
        self.requested.borrow_mut().clear();
    }
    fn replace(&self, url: &str, body: Vec<u8>) {
        self.files.borrow_mut().insert(url.to_owned(), body);
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

// ---- artifacts shaped like a Windows installer ---------------------------

/// A file that is not a gzipped tarball and must never be opened as one.
///
/// Starts `MZ`, like every PE, and carries a large shared body with a small
/// changed region — which is what a real installer rebuild looks like when only
/// the application binary inside it changed, and is what makes a patch worth
/// generating at all.
fn installer(dir: &Path, version: &str, seed: u32) -> PathBuf {
    let mut bytes = b"MZ\x90\x00".to_vec();
    bytes.extend(
        (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(7) % 256) as u8),
    );
    // The part that differs between versions, in the middle of the file.
    let changed: Vec<u8> = (0..4_096u32)
        .map(|i| (i.wrapping_mul(2_246_822_519).wrapping_add(seed) % 256) as u8)
        .collect();
    bytes[150_000..150_000 + changed.len()].copy_from_slice(&changed);
    bytes.extend_from_slice(version.as_bytes());

    let out = dir.join(format!("App_{version}_x64-setup.exe"));
    std::fs::write(&out, &bytes).expect("write installer");
    out
}

fn keypair() -> KeyPair {
    KeyPair::generate_encrypted_keypair(Some(String::new())).expect("generate keypair")
}

fn pubkey_b64(pair: &KeyPair) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(pair.pk.to_box().expect("box pk").into_string())
}

/// Three real releases and the server that serves them.
///
/// Three rather than two because the cache state machine cannot be exercised
/// with fewer: one transition fills the cache and the second uses it. A
/// two-version fixture can only ever demonstrate the first.
struct World {
    server: FakeServer,
    pubkey: String,
    installers: HashMap<String, PathBuf>,
    manifests: HashMap<String, Manifest>,
    patch_sizes: HashMap<String, u64>,
}

impl World {
    /// The update `to`, as Tauri's own check would have resolved it.
    fn identity(&self, from: &str, to: &str) -> UpdateIdentity {
        let manifest = &self.manifests[to];
        UpdateIdentity::new(
            from,
            to,
            // Tauri's `Update.target` is `updater_os()`, not the manifest key.
            if cfg!(windows) { "windows" } else { "linux" },
            &installer_url(to),
            &manifest.platforms[&current_platform()].signature,
            manifest.to_json().expect("serialise"),
        )
    }

    fn signature(&self, version: &str) -> String {
        self.manifests[version].platforms[&current_platform()]
            .signature
            .clone()
    }

    fn bytes(&self, version: &str) -> Vec<u8> {
        std::fs::read(&self.installers[version]).expect("read installer")
    }
}

/// Build 1.0.0, 1.0.1 and 1.0.2 and release the two transitions between them.
fn world(dir: &Path, pair: &KeyPair) -> World {
    let key = SigningKey::from_str(&pair.sk.to_box(None).expect("box key").into_string(), None)
        .expect("load key");

    let mut installers = HashMap::new();
    for (i, version) in ["1.0.0", "1.0.1", "1.0.2"].iter().enumerate() {
        installers.insert(
            (*version).to_owned(),
            installer(dir, version, 1_000 + i as u32 * 7),
        );
    }

    // Distinct bytes, or every assertion below passes vacuously.
    for (a, b) in [("1.0.0", "1.0.1"), ("1.0.1", "1.0.2"), ("1.0.0", "1.0.2")] {
        assert_ne!(
            std::fs::read(&installers[a]).expect("read"),
            std::fs::read(&installers[b]).expect("read"),
            "{a} and {b} must differ"
        );
    }

    let mut files = HashMap::new();
    let mut manifests = HashMap::new();
    let mut patch_sizes = HashMap::new();

    for (from, to) in [("1.0.0", "1.0.1"), ("1.0.1", "1.0.2")] {
        let patch_out = dir.join(format!("{from}-to-{to}.zst"));
        let (manifest, summary) = build_release(
            &ReleaseRequest {
                platform: &current_platform(),
                version: to,
                new_installer: &installers[to],
                installer_url: &installer_url(to),
                notes: None,
                pub_date: None,
                app_id: APP_ID,
                predecessor: Some(Predecessor {
                    from_version: from,
                    installer: &installers[from],
                    patch_url: &patch_url(from, to),
                    patch_out: &patch_out,
                    // No tar layer: these artifacts are not tarballs, which is
                    // the entire point of the representation under test.
                    tar_layer: None,
                }),
                allow_insecure_urls: false,
            },
            &key,
            None,
        )
        .expect("release should build");

        assert!(
            summary.patch_size.is_some(),
            "the fixture must publish a direct patch"
        );
        patch_sizes.insert(to.to_owned(), summary.patch_size.expect("patch size"));

        files.insert(
            patch_url(from, to),
            std::fs::read(&patch_out).expect("read patch"),
        );
        files.insert(
            installer_url(to),
            std::fs::read(&installers[to]).expect("read installer"),
        );
        manifests.insert(to.to_owned(), manifest);
    }

    World {
        server: FakeServer {
            files: RefCell::new(files),
            requested: RefCell::new(Vec::new()),
        },
        pubkey: pubkey_b64(pair),
        installers,
        manifests,
        patch_sizes,
    }
}

// ---- the cache a Windows client actually opens ---------------------------

fn opaque_namespace(pubkey: &str) -> Namespace {
    Namespace {
        bundle_id: APP_ID.to_owned(),
        platform: current_platform(),
        arch: std::env::consts::ARCH.to_owned(),
        pubkey_fingerprint: FileHash::of_bytes(pubkey.as_bytes()).to_hex(),
        representation: REPRESENTATION_OPAQUE_V1.to_owned(),
        recompression: RECOMPRESSION_NONE.to_owned(),
    }
}

fn open_cache(root: &Path, pubkey: &str) -> ArtifactCache {
    let cache = ArtifactCache::open(root, opaque_namespace(pubkey), CacheLimits::default())
        .expect("open cache");
    assert_eq!(cache.representation(), CachedRepresentation::Opaque);
    cache
}

fn run(
    w: &World,
    from: &str,
    to: &str,
    cache: Option<&ArtifactCache>,
    handoff: &RecordingHandoff,
    work: &Path,
) -> tauri_plugin_updater_delta::Result<Outcome> {
    run_update(
        &w.identity(from, to),
        &Context {
            pubkey: &w.pubkey,
            // What a normal build passes. The managed cache is the only source
            // of a direct base in the shipping API.
            base: None,
            cache,
            app_id: APP_ID,
            work_dir: work,
            limits: Limits::default(),
        },
        &w.server,
        handoff,
    )
}

fn plan(
    w: &World,
    from: &str,
    to: &str,
    cache: Option<&ArtifactCache>,
    work: &Path,
) -> UpdateSource {
    plan_update(
        &w.identity(from, to),
        &PlanContext {
            base: None,
            cache,
            pubkey: &w.pubkey,
            app_id: APP_ID,
            work_dir: work,
            limits: Limits::default(),
        },
        &w.server,
    )
}

/// The reason and attempt record from a fallback, or a panic naming what it was.
fn full_fallback(source: &UpdateSource) -> (String, bool) {
    match source {
        UpdateSource::Full {
            reason, attempted, ..
        } => (
            reason
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "<no reason>".to_owned()),
            attempted.direct_delta,
        ),
        other => panic!("expected a Full fallback, got {}", other.path_name()),
    }
}

// ---- the ladder ----------------------------------------------------------

#[test]
fn the_full_relaunch_direct_delta_ladder_completes() {
    // The whole claim, in one test: Full, promotion on relaunch, then a delta
    // selected from the artifact that Full staged. This is the sequence the
    // audit's Windows acceptance criteria describe, with the network and the
    // installer faked and everything else real.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    let work = dir.path().join("work");

    // -- transition 1: nothing cached, so Full, and it must stage cleanly.
    let handoff = RecordingHandoff::default();
    let (outcome, cache_error) = run_update_with_cache_diagnostic(
        &w.identity("1.0.0", "1.0.1"),
        &Context {
            pubkey: &w.pubkey,
            base: None,
            cache: Some(&cache),
            app_id: APP_ID,
            work_dir: &work,
            limits: Limits::default(),
        },
        &w.server,
        &handoff,
    )
    .expect("the first update must succeed");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert_eq!(
        cache_error, None,
        "an opaque installer must persist without a diagnostic; this is the \
         failure that kept every Windows update cache-cold"
    );
    assert_eq!(handoff.installed.borrow()[0], w.bytes("1.0.1"));

    let staged = cache.state().expect("state").pending.expect("staged 1.0.1");
    assert_eq!(staged.version, "1.0.1");
    assert!(staged.tar.is_none(), "there is no tar in an installer");
    assert!(
        cache.state().expect("state").active.is_none(),
        "install() returning Ok does not license a promotion"
    );

    // -- relaunch: the running version is what promotes, nothing else.
    assert_eq!(
        cache.reconcile("1.0.1").expect("reconcile"),
        Reconciliation::Promoted {
            version: "1.0.1".to_owned()
        }
    );

    // -- transition 2: the cached base is now the direct patch's base.
    w.server.forget();
    let handoff = RecordingHandoff::default();
    let outcome = run(&w, "1.0.1", "1.0.2", Some(&cache), &handoff, &work)
        .expect("the second update must succeed");

    match outcome {
        Outcome::InstalledFromDelta {
            downloaded,
            saved_against,
        } => {
            assert_eq!(downloaded, w.patch_sizes["1.0.2"]);
            assert_eq!(saved_against, w.bytes("1.0.2").len() as u64);
        }
        other => panic!("expected a direct delta, got {}", other.path_name()),
    }

    // Installed bytes alone cannot tell a delta from a full download. The
    // request log can.
    assert!(w.server.fetched(&patch_url("1.0.1", "1.0.2")));
    assert!(
        !w.server.fetched(&installer_url("1.0.2")),
        "the full artifact must not have been fetched"
    );
    assert_eq!(handoff.installed.borrow()[0], w.bytes("1.0.2"));

    // And the cycle repeats: 1.0.2 is staged, 1.0.1 is still the base.
    let state = cache.state().expect("state");
    assert_eq!(state.pending.expect("staged").version, "1.0.2");
    assert_eq!(state.active.expect("active").version, "1.0.1");
}

#[test]
fn a_reconstructed_installer_is_byte_identical_to_the_published_one() {
    // The gate that makes the whole scheme safe, restated for this path: what
    // the delta produces is not "equivalent to" the release, it is the release.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    let handoff = RecordingHandoff::default();
    let outcome = run(
        &w,
        "1.0.1",
        "1.0.2",
        Some(&cache),
        &handoff,
        &dir.path().join("work"),
    )
    .expect("update");

    assert_eq!(outcome.path_name(), "delta");
    assert_eq!(
        handoff.installed.borrow()[0],
        w.bytes("1.0.2"),
        "the installer must receive the exact published bytes"
    );
}

/// Put `version` into the cache as ACTIVE, through the real staging path.
fn seed_active(cache: &ArtifactCache, w: &World, pair: &KeyPair, version: &str) {
    let bytes = w.bytes(version);
    let signature = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(None, &pair.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string(),
    );
    let verified = tauri_updater_delta_core::verify_artifact(bytes, &signature, &w.pubkey)
        .expect("the seeded base must verify");
    cache
        .stage_pending(version, &verified, &signature)
        .expect("stage");
    assert_eq!(
        cache.reconcile(version).expect("reconcile"),
        Reconciliation::Promoted {
            version: version.to_owned()
        }
    );
}

// ---- everything that must fall back, and be seen to have tried -----------

#[test]
fn a_cold_cache_reaches_the_direct_path_and_declines() {
    // No ACTIVE entry at all: the ordinary state before a client's first
    // update. What matters is that the direct path is *reached* and declines
    // for a nameable reason, rather than never being reached -- which is how a
    // delta updater that cannot delta looks exactly like one that can.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);

    let source = plan(&w, "1.0.1", "1.0.2", Some(&cache), &dir.path().join("work"));
    let (reason, attempted) = full_fallback(&source);
    assert!(attempted, "the direct path must have been reached");
    assert!(reason.contains("no cached base"), "got: {reason}");
    assert!(
        !w.server.fetched(&patch_url("1.0.1", "1.0.2")),
        "a missing base must be found before a patch is paid for"
    );
}

#[test]
fn no_cache_at_all_falls_back() {
    // The cache could not be opened — an unwritable directory, a corrupt store.
    // The update still completes; only the optimisation is lost.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);

    let handoff = RecordingHandoff::default();
    let outcome = run(
        &w,
        "1.0.1",
        "1.0.2",
        None,
        &handoff,
        &dir.path().join("work"),
    )
    .expect("an update with no cache must still install");
    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert_eq!(handoff.installed.borrow()[0], w.bytes("1.0.2"));
}

#[test]
fn the_wrong_base_is_refused_before_the_patch_is_downloaded() {
    // The cache holds 1.0.0 and the release patches from 1.0.1. Without the
    // declared base in the manifest this would download the patch, apply it,
    // and discover the mismatch from a failed target digest — correct, and paid
    // for in bytes.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.0");

    // Tauri reports the running version as 1.0.1 while the cache holds 1.0.0.
    let source = plan(&w, "1.0.1", "1.0.2", Some(&cache), &dir.path().join("work"));
    let (reason, attempted) = full_fallback(&source);
    assert!(attempted);
    assert!(
        reason.contains("cached base installer"),
        "the mismatch should name the base, got: {reason}"
    );
    assert!(
        !w.server.fetched(&patch_url("1.0.1", "1.0.2")),
        "a wrong base must be detected before the patch is fetched"
    );
}

#[test]
fn a_patch_with_no_declared_base_does_not_take_the_cached_path() {
    // An older release, or another tool's manifest. The base cannot be checked,
    // so the choice is to download the patch on the hope the cache holds its
    // base, or to fall back. Falling back is cheaper and is what happens.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    seed_active(
        &open_cache(&dir.path().join("cache"), &w.pubkey),
        &w,
        &pair,
        "1.0.1",
    );

    let manifest = w.manifests.get_mut("1.0.2").expect("manifest");
    let entry = manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(&current_platform())
        .expect("platform");
    let patch = entry.patches.get_mut("1.0.1").expect("patch");
    patch.base_installer_blake3 = None;
    patch.base_installer_size = None;
    manifest
        .validate()
        .expect("a patch with no declared base is still a valid manifest");

    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    let source = plan(&w, "1.0.1", "1.0.2", Some(&cache), &dir.path().join("work"));
    let (reason, attempted) = full_fallback(&source);
    assert!(attempted);
    assert!(reason.contains("declares no base"), "got: {reason}");
    assert!(!w.server.fetched(&patch_url("1.0.1", "1.0.2")));
}

#[test]
fn a_corrupt_patch_falls_back_to_a_verified_full_download() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    // Same length, different bytes: only the digest catches it.
    let url = patch_url("1.0.1", "1.0.2");
    let mut body = w.server.files.borrow()[&url].clone();
    let mid = body.len() / 2;
    body[mid] ^= 0xFF;
    w.server.replace(&url, body);

    let handoff = RecordingHandoff::default();
    let outcome = run(
        &w,
        "1.0.1",
        "1.0.2",
        Some(&cache),
        &handoff,
        &dir.path().join("work"),
    )
    .expect("a corrupt patch must degrade, not fail");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert!(w.server.fetched(&url), "the patch was tried");
    assert!(w.server.fetched(&installer_url("1.0.2")));
    assert_eq!(handoff.installed.borrow()[0], w.bytes("1.0.2"));
}

#[test]
fn a_truncated_patch_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    let url = patch_url("1.0.1", "1.0.2");
    let body = w.server.files.borrow()[&url].clone();
    w.server.replace(&url, body[..body.len() / 2].to_vec());

    let handoff = RecordingHandoff::default();
    let outcome = run(
        &w,
        "1.0.1",
        "1.0.2",
        Some(&cache),
        &handoff,
        &dir.path().join("work"),
    )
    .expect("a truncated patch must degrade");
    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert_eq!(handoff.installed.borrow()[0], w.bytes("1.0.2"));
}

#[test]
fn a_missing_patch_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    w.server
        .files
        .borrow_mut()
        .remove(&patch_url("1.0.1", "1.0.2"));

    let handoff = RecordingHandoff::default();
    let outcome = run(
        &w,
        "1.0.1",
        "1.0.2",
        Some(&cache),
        &handoff,
        &dir.path().join("work"),
    )
    .expect("a 404 on the patch must degrade");
    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert_eq!(handoff.installed.borrow()[0], w.bytes("1.0.2"));
}

#[test]
fn a_corrupt_cached_blob_falls_back_to_a_verified_full_download() {
    // The cache directory is writable by the user and by anything that has ever
    // run as the user, so a cached artifact is untrusted on every reuse. Here it
    // fails re-verification rather than being trusted because it is on disk.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache_dir = dir.path().join("cache");
    let cache = open_cache(&cache_dir, &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    let blob = std::fs::read_dir(cache_dir.join("blobs"))
        .expect("blobs")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_file())
        .expect("one blob");
    let mut bytes = std::fs::read(&blob).expect("read blob");
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&blob, &bytes).expect("corrupt blob");

    let handoff = RecordingHandoff::default();
    let outcome = run(
        &w,
        "1.0.1",
        "1.0.2",
        Some(&cache),
        &handoff,
        &dir.path().join("work"),
    )
    .expect("a corrupt cache must degrade, not fail");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert!(
        !w.server.fetched(&patch_url("1.0.1", "1.0.2")),
        "a base that does not re-verify must be rejected before the patch"
    );
    assert_eq!(handoff.installed.borrow()[0], w.bytes("1.0.2"));
}

// ---- what must NOT fall back ---------------------------------------------

#[test]
fn a_tampered_final_artifact_installs_nothing() {
    // The reconstruction matched the manifest's digest; the signature did not
    // accept it. That is not a transfer problem, and the full path is described
    // by the same unsigned document, so retrying there would grant a second
    // attempt rather than a safer one (`docs/DECISIONS.md` #11).
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    // A different key signs the same bytes: a real signature over a real
    // artifact, issued by someone who is not the release.
    let other = keypair();
    let bytes = w.bytes("1.0.2");
    let forged = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(None, &other.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string(),
    );
    let mut identity_json = w.manifests["1.0.2"].to_json().expect("serialise");
    identity_json = identity_json.replace(&w.signature("1.0.2"), &forged);

    let identity = UpdateIdentity::new(
        "1.0.1",
        "1.0.2",
        if cfg!(windows) { "windows" } else { "linux" },
        &installer_url("1.0.2"),
        &forged,
        identity_json,
    );

    let handoff = RecordingHandoff::default();
    let result = run_update(
        &identity,
        &Context {
            pubkey: &w.pubkey,
            base: None,
            cache: Some(&cache),
            app_id: APP_ID,
            work_dir: &dir.path().join("work"),
            limits: Limits::default(),
        },
        &w.server,
        &handoff,
    );

    assert!(matches!(result, Err(Error::Signature(_))), "got {result:?}");
    assert!(
        handoff.installed.borrow().is_empty(),
        "nothing may be installed when the signature does not hold"
    );
}

#[test]
fn an_artifact_signed_for_another_application_installs_nothing() {
    // One key, two products. The authenticated release identity binds the
    // artifact to an app id, and a contradiction is refused rather than
    // downloaded in full.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    let handoff = RecordingHandoff::default();
    let result = run_update(
        &w.identity("1.0.1", "1.0.2"),
        &Context {
            pubkey: &w.pubkey,
            base: None,
            cache: Some(&cache),
            app_id: "dev.example.a-different-product",
            work_dir: &dir.path().join("work"),
            limits: Limits::default(),
        },
        &w.server,
        &handoff,
    );

    assert!(matches!(result, Err(Error::Refused(_))), "got {result:?}");
    assert!(handoff.installed.borrow().is_empty());
}

#[test]
fn a_downgrade_installs_nothing_and_downloads_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    let handoff = RecordingHandoff::default();
    // Running 1.0.2, offered 1.0.1.
    let result = run(
        &w,
        "1.0.2",
        "1.0.1",
        Some(&cache),
        &handoff,
        &dir.path().join("work"),
    );

    assert!(matches!(result, Err(Error::Refused(_))), "got {result:?}");
    assert!(handoff.installed.borrow().is_empty());
    assert!(w.server.requested.borrow().is_empty());
}

// ---- the representation boundary -----------------------------------------

#[test]
fn an_app_tar_gz_cache_never_supplies_a_direct_base() {
    // macOS behaviour, unchanged and asserted. A direct patch between two gzip
    // streams measured 95-96% of a full download (`docs/DECISIONS.md` #15), so
    // taking it when the tar path declined would download a patch the size of
    // the artifact and call it an optimisation -- and would relabel a failed
    // TarDelta as a successful DirectDelta, which is the mislabelling #22 is
    // about.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);

    let mut ns = opaque_namespace(&w.pubkey);
    ns.representation = REPRESENTATION_APP_TAR_GZ_V1.to_owned();
    ns.recompression = RECOMPRESSION_TAURI_APP_TAR_GZ_V1.to_owned();
    let cache =
        ArtifactCache::open(dir.path().join("cache"), ns, CacheLimits::default()).expect("open");
    assert_eq!(cache.representation(), CachedRepresentation::AppTarGz);

    let source = plan(&w, "1.0.1", "1.0.2", Some(&cache), &dir.path().join("work"));
    let (reason, attempted) = full_fallback(&source);
    assert!(attempted, "the direct path is still reached and declines");
    assert!(reason.contains("cheaper path"), "got: {reason}");
    assert!(!w.server.fetched(&patch_url("1.0.1", "1.0.2")));
}

#[test]
fn an_oversized_target_is_refused_before_anything_is_fetched() {
    // The manifest is unauthenticated, so its idea of "how big is the target"
    // is a request rather than a fact.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair, "1.0.1");

    let source = plan_update(
        &w.identity("1.0.1", "1.0.2"),
        &PlanContext {
            base: None,
            cache: Some(&cache),
            pubkey: &w.pubkey,
            app_id: APP_ID,
            work_dir: &dir.path().join("work"),
            limits: Limits {
                max_target_bytes: 1_024,
                ..Limits::default()
            },
        },
        &w.server,
    );

    let (reason, attempted) = full_fallback(&source);
    assert!(
        !attempted,
        "the ceiling is checked before either delta path runs"
    );
    assert!(
        reason.contains("1024") || reason.to_lowercase().contains("size"),
        "got: {reason}"
    );
    assert!(w.server.requested.borrow().is_empty());
}
