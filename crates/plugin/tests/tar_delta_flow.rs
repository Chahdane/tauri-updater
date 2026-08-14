//! The tar-layer path, end to end, against a manifest the real release tool made.
//!
//! Nothing here is hand-built. `delta-release` generates the tar patch and the
//! metadata; the cache is a real [`ArtifactCache`] on disk; the artifacts are
//! real `.app.tar.gz` files written by `tar::Builder` into a `GzEncoder`, which
//! is the shape `tauri-bundler` produces. Only the network and the installer are
//! fakes, because those are the two boundaries the design makes injectable.
//!
//! # What these tests are for
//!
//! Two different things, and keeping them apart matters.
//!
//! The happy path proves the **mechanism runs**: a cached base is expanded,
//! patched, recompressed and verified. Asserting the installed bytes alone would
//! not prove that, because a full download produces the same bytes. Every
//! assertion here names the path.
//!
//! The failure cases prove the **fallback is real**, and each one asserts that
//! the tar path was *attempted* before it fell back. A fallback test that passes
//! because the path was never reached proves nothing at all — which is exactly
//! how a delta updater that never deltaed passed four gates of green tests
//! (`docs/DECISIONS.md` #22).

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

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
use tauri_updater_delta_core::cache::{ArtifactCache, CacheLimits, Namespace, Reconciliation};
use tauri_updater_delta_core::client::{plan_update, Fetch, PlanContext, UpdateSource};
use tauri_updater_delta_core::manifest::{
    Manifest, RECOMPRESSION_TAURI_APP_TAR_GZ_V1, REPRESENTATION_APP_TAR_GZ_V1,
};
use tauri_updater_delta_core::{FileHash, Limits, UpdateIdentity, VerifiedArtifact};
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{build_release, ReleaseRequest, TarLayerOptions};

const INSTALLER_URL: &str = "https://example.com/App.app.tar.gz";
const PATCH_URL: &str = "https://example.com/direct.zst";
const TAR_PATCH_URL: &str = "https://example.com/tar.zst";

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
    fn replace(&self, url: &str, body: Vec<u8>) {
        self.files.borrow_mut().insert(url.to_owned(), body);
    }
}

#[derive(Default)]
struct RecordingHandoff {
    installed: RefCell<Vec<Vec<u8>>>,
    /// Make the installer fail, as a real one can.
    fail: bool,
}

impl InstallHandoff for RecordingHandoff {
    fn install(&self, artifact: &VerifiedArtifact) -> tauri_plugin_updater_delta::Result<()> {
        if self.fail {
            return Err(Error::Io("the installer refused".to_owned()));
        }
        self.installed
            .borrow_mut()
            .push(artifact.as_bytes().to_vec());
        Ok(())
    }
}

// ---- real artifacts ------------------------------------------------------

/// A real `.app.tar.gz`, written the way `tauri-bundler` writes one.
fn bundle(dir: &Path, name: &str, binary: &[u8]) -> PathBuf {
    let root = dir.join(format!("{name}-src"));
    std::fs::create_dir_all(root.join("Contents/MacOS")).expect("mkdir");
    std::fs::write(root.join("Contents/Info.plist"), b"<plist/>").expect("write plist");
    let mut f = std::fs::File::create(root.join("Contents/MacOS/app")).expect("create");
    f.write_all(binary).expect("write binary");
    drop(f);

    let out = dir.join(format!("{name}.app.tar.gz"));
    let encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&out).expect("create"),
        flate2::Compression::default(),
    );
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    builder
        .append_dir_all("DeltaExample.app", &root)
        .expect("append");
    builder
        .into_inner()
        .expect("finish tar")
        .finish()
        .expect("finish gzip");
    out
}

fn binary(seed: u32, len: usize) -> Vec<u8> {
    (0..len as u32)
        .map(|i| (i.wrapping_mul(2654435761).wrapping_add(seed) % 256) as u8)
        .collect()
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
    old: PathBuf,
    new: PathBuf,
    manifest: Manifest,
    manifest_json: String,
    signature: String,
}

impl World {
    fn identity(&self, current_version: &str) -> UpdateIdentity {
        UpdateIdentity::new(
            current_version,
            "1.0.1",
            "darwin",
            INSTALLER_URL,
            &self.signature,
            &self.manifest_json,
        )
    }

    fn released_bytes(&self) -> Vec<u8> {
        std::fs::read(&self.new).expect("read released artifact")
    }
}

/// Run the real release tooling over two real bundles, with a tar layer.
fn world(dir: &Path, pair: &KeyPair) -> World {
    let mut old_bin = binary(1, 300_000);
    let mut new_bin = old_bin.clone();
    new_bin[150_000..150_128].copy_from_slice(&binary(9, 128));
    old_bin.extend_from_slice(b"version one");
    new_bin.extend_from_slice(b"version two, a bit longer");

    let old = bundle(dir, "old", &old_bin);
    let new = bundle(dir, "new", &new_bin);

    let key = SigningKey::from_str(&pair.sk.to_box(None).expect("box key").into_string(), None)
        .expect("load key");

    let direct_patch = dir.join("direct.zst");
    let tar_patch = dir.join("tar.zst");

    let (manifest, summary) = build_release(
        &ReleaseRequest {
            platform: &platform(),
            version: "1.0.1",
            from_version: "1.0.0",
            previous_installer: &old,
            new_installer: &new,
            installer_url: INSTALLER_URL,
            patch_url: PATCH_URL,
            patch_out: &direct_patch,
            notes: None,
            pub_date: None,
            app_id: "dev.example.testapp",
            tar_layer: Some(TarLayerOptions {
                patch_url: TAR_PATCH_URL,
                patch_out: &tar_patch,
                work_dir: None,
                max_tar_bytes: 64 * 1024 * 1024,
                // The whole fixture is pointless if the layer silently did not
                // get built, so make that a failure rather than a surprise.
                required: true,
            }),
        },
        &key,
        None,
    )
    .expect("release should build");

    assert!(
        summary.tar_patch_size.is_some(),
        "the fixture must have a tar layer"
    );

    let files = HashMap::from([
        (
            PATCH_URL.to_owned(),
            std::fs::read(&direct_patch).expect("read direct patch"),
        ),
        (
            TAR_PATCH_URL.to_owned(),
            std::fs::read(&tar_patch).expect("read tar patch"),
        ),
        (
            INSTALLER_URL.to_owned(),
            std::fs::read(&new).expect("read installer"),
        ),
    ]);

    World {
        server: FakeServer {
            files: RefCell::new(files),
            requested: RefCell::new(Vec::new()),
        },
        pubkey: pubkey_b64(pair),
        old,
        new,
        signature: manifest.platforms[&platform()].signature.clone(),
        manifest_json: manifest.to_json().expect("serialise"),
        manifest,
    }
}

// ---- a real cache, populated the way the flow populates one -------------

fn namespace(pubkey: &str) -> Namespace {
    Namespace {
        bundle_id: "com.example.delta".to_owned(),
        platform: platform().to_owned(),
        arch: "aarch64".to_owned(),
        pubkey_fingerprint: FileHash::of_bytes(pubkey.as_bytes()).to_hex(),
        representation: REPRESENTATION_APP_TAR_GZ_V1.to_owned(),
        recompression: RECOMPRESSION_TAURI_APP_TAR_GZ_V1.to_owned(),
    }
}

fn open_cache(root: &Path, pubkey: &str) -> ArtifactCache {
    ArtifactCache::open(root, namespace(pubkey), CacheLimits::default()).expect("open cache")
}

/// Put 1.0.0 into the cache as ACTIVE, through the real staging path.
fn seed_active(cache: &ArtifactCache, w: &World, pair: &KeyPair) {
    let bytes = std::fs::read(&w.old).expect("read old artifact");
    let signature = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(None, &pair.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string(),
    );
    let verified = tauri_updater_delta_core::verify_artifact(bytes, &signature, &w.pubkey)
        .expect("the seeded base must verify");
    cache
        .stage_pending("1.0.0", &verified, &signature)
        .expect("stage");
    assert_eq!(
        cache.reconcile("1.0.0").expect("reconcile"),
        Reconciliation::Promoted {
            version: "1.0.0".to_owned()
        }
    );
}

fn run(
    w: &World,
    cache: Option<&ArtifactCache>,
    handoff: &RecordingHandoff,
    work: &Path,
) -> tauri_plugin_updater_delta::Result<Outcome> {
    run_update(
        &w.identity("1.0.0"),
        &Context {
            pubkey: &w.pubkey,
            // Deliberately no direct base: these tests are about the tar path,
            // and leaving the direct path unavailable means a tar failure shows
            // up as Full rather than silently succeeding down the other branch.
            base: None,
            cache,
            app_id: "dev.example.testapp",
            work_dir: work,
            limits: Limits::default(),
        },
        &w.server,
        handoff,
    )
}

/// Plan without installing, so a test can inspect which paths were attempted.
fn plan(w: &World, cache: Option<&ArtifactCache>, work: &Path) -> UpdateSource {
    plan_with_limits(w, cache, work, Limits::default())
}

/// [`plan`] with the host's ceilings set explicitly, for the resource tests.
fn plan_with_limits(
    w: &World,
    cache: Option<&ArtifactCache>,
    work: &Path,
    limits: Limits,
) -> UpdateSource {
    plan_update(
        &w.identity("1.0.0"),
        &PlanContext {
            base: None,
            cache,
            pubkey: &w.pubkey,
            app_id: "dev.example.testapp",
            work_dir: work,
            limits,
        },
        &w.server,
    )
}

/// Rewrite the tar layer in place and re-serialise, which is what a hostile
/// update server gets to do to every one of these numbers.
fn edit_tar_layer(
    w: &mut World,
    edit: impl FnOnce(&mut tauri_updater_delta_core::manifest::TarLayer),
) {
    let entry = w
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(&platform())
        .expect("platform");
    edit(entry.tar_layer.as_mut().expect("tar layer"));
    w.manifest_json = w.manifest.to_json().expect("serialise");
}

/// Assert a fallback happened *after* the tar path genuinely ran, and return
/// the reason so a caller can say which gate caught it.
fn assert_fell_back_from_tar(source: &UpdateSource) -> String {
    let UpdateSource::Full {
        reason, attempted, ..
    } = source
    else {
        panic!("expected a full download, got {source:?}");
    };
    assert!(
        attempted.tar_delta,
        "the tar path must have been attempted, or this test proves nothing"
    );
    reason
        .as_ref()
        .map(|r| r.to_string())
        .expect("an attempted-and-failed tar path must report why")
}

// ---- the happy path ------------------------------------------------------

#[test]
fn a_valid_cache_takes_the_tar_path_and_installs_the_released_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let handoff = RecordingHandoff::default();
    let outcome = run(&w, Some(&cache), &handoff, &dir.path().join("work")).expect("update");

    let Outcome::InstalledFromTarDelta {
        downloaded,
        saved_against,
    } = outcome
    else {
        panic!("expected the tar path, got {outcome:?}");
    };
    assert_eq!(outcome.path_name(), "tar-delta");

    // The tar patch was fetched and the full artifact was not. Without this,
    // "it installed the right bytes" is equally true of a full download.
    assert!(w.server.fetched(TAR_PATCH_URL));
    assert!(
        !w.server.fetched(INSTALLER_URL),
        "the tar path must not download the full artifact"
    );
    assert!(
        !w.server.fetched(PATCH_URL),
        "the direct patch must not have been used"
    );

    assert!(
        downloaded < saved_against / 2,
        "a tar patch of {downloaded} against a {saved_against}-byte artifact is not a saving"
    );

    let installed = handoff.installed.borrow();
    assert_eq!(installed.len(), 1);
    assert_eq!(
        installed[0],
        w.released_bytes(),
        "the recompressed artifact must be byte-identical to the published one"
    );
}

#[test]
fn the_installed_artifact_is_staged_as_pending_and_promoted_on_the_next_launch() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let handoff = RecordingHandoff::default();
    run(&w, Some(&cache), &handoff, &dir.path().join("work")).expect("update");

    // Staged, not promoted: the app is still running 1.0.0.
    let state = cache.state().expect("state");
    assert_eq!(state.active.expect("active").version, "1.0.0");
    let pending = state.pending.expect("pending");
    assert_eq!(pending.version, "1.0.1");
    assert_eq!(
        pending.compressed_blake3,
        FileHash::of_bytes(&w.released_bytes()).to_hex(),
        "the staged artifact must be the one that was installed"
    );

    // The app comes back up as 1.0.1.
    assert_eq!(
        cache.reconcile("1.0.1").expect("reconcile"),
        Reconciliation::Promoted {
            version: "1.0.1".to_owned()
        }
    );
    let base = cache.active(&w.pubkey).expect("active").expect("base");
    assert_eq!(base.entry.version, "1.0.1");
    assert_eq!(base.artifact.as_bytes(), &w.released_bytes()[..]);
}

#[test]
fn a_full_download_populates_the_cache_for_next_time() {
    // The first update on a fresh installation. It cannot use the tar path --
    // there is no base -- and its whole contribution to the next update is
    // leaving one behind.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);

    let handoff = RecordingHandoff::default();
    let outcome = run(&w, Some(&cache), &handoff, &dir.path().join("work")).expect("update");
    assert_eq!(outcome, Outcome::InstalledFromFullDownload);
    assert!(w.server.fetched(INSTALLER_URL));

    let pending = cache.state().expect("state").pending.expect("pending");
    assert_eq!(pending.version, "1.0.1");
    cache.reconcile("1.0.1").expect("reconcile");
    assert!(cache.active(&w.pubkey).expect("active").is_some());
}

#[test]
fn the_direct_patch_still_works_when_there_is_no_cache_at_all() {
    // The pre-cache behaviour, unchanged. A host that keeps no cache gets
    // exactly what it got before the tar layer existed.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);

    let handoff = RecordingHandoff::default();
    let outcome = run_update(
        &w.identity("1.0.0"),
        &Context {
            pubkey: &w.pubkey,
            base: Some(&w.old),
            cache: None,
            app_id: "dev.example.testapp",
            work_dir: &dir.path().join("work"),
            limits: Limits::default(),
        },
        &w.server,
        &handoff,
    )
    .expect("update");

    assert!(
        matches!(outcome, Outcome::InstalledFromDelta { .. }),
        "expected the direct path, got {outcome:?}"
    );
    assert!(w.server.fetched(PATCH_URL));
    assert!(!w.server.fetched(TAR_PATCH_URL));
    assert_eq!(handoff.installed.borrow()[0], w.released_bytes());
}

// ---- everything that must fall back --------------------------------------

#[test]
fn a_missing_active_cache_falls_back_to_a_full_download() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    // No seed: the cache is empty.

    let source = plan(&w, Some(&cache), &dir.path().join("work"));
    let _ = assert_fell_back_from_tar(&source);
}

#[test]
fn a_pending_entry_is_not_usable_as_a_base() {
    // Staged but never promoted, because the app never came back up running it.
    // Using it would patch against something the user is not running.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);

    let bytes = std::fs::read(&w.old).expect("read");
    let signature = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(None, &pair.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string(),
    );
    let verified =
        tauri_updater_delta_core::verify_artifact(bytes, &signature, &w.pubkey).expect("verify");
    cache
        .stage_pending("1.0.0", &verified, &signature)
        .expect("stage");

    let _ = assert_fell_back_from_tar(&plan(&w, Some(&cache), &dir.path().join("work")));
}

#[test]
fn a_corrupt_cached_gzip_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache_root = dir.path().join("cache");
    let cache = open_cache(&cache_root, &w.pubkey);
    seed_active(&cache, &w, &pair);

    // Same length, different bytes: only the digest can catch it.
    let digest = cache
        .state()
        .expect("state")
        .active
        .expect("active")
        .compressed_blake3;
    let blob = cache_root.join("blobs").join(format!("{digest}.tar.gz"));
    let mut bytes = std::fs::read(&blob).expect("read blob");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xFF;
    std::fs::write(&blob, &bytes).expect("corrupt");

    let _ = assert_fell_back_from_tar(&plan(&w, Some(&cache), &dir.path().join("work")));
}

#[test]
fn a_cache_signed_by_a_key_this_build_no_longer_trusts_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache_root = dir.path().join("cache");

    // Seeded and promoted under the release key...
    let cache = open_cache(&cache_root, &w.pubkey);
    seed_active(&cache, &w, &pair);

    // ...then the entry's stored signature is replaced with one from another
    // key. The namespace still matches, so only the signature re-check can
    // refuse it -- which is the point of re-verifying on every reuse.
    let other = keypair();
    let bytes = std::fs::read(&w.old).expect("read");
    let foreign = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(None, &other.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string(),
    );
    let generations: Vec<PathBuf> = std::fs::read_dir(&cache_root)
        .expect("read cache dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("state."))
                .unwrap_or(false)
        })
        .collect();
    let newest = generations
        .iter()
        .max_by_key(|p| p.to_string_lossy().to_string())
        .expect("a state file");
    let text = std::fs::read_to_string(newest).expect("read state");
    let mut state: serde_json::Value = serde_json::from_str(&text).expect("parse state");
    state["active"]["signature"] = serde_json::Value::String(foreign);
    std::fs::write(
        newest,
        serde_json::to_string_pretty(&state).expect("serialise"),
    )
    .expect("write state");

    let reopened = open_cache(&cache_root, &w.pubkey);
    let _ = assert_fell_back_from_tar(&plan(&w, Some(&reopened), &dir.path().join("work")));
}

#[test]
fn a_cache_holding_the_wrong_base_falls_back() {
    // A perfectly valid artifact for a version the patch was not made against.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);

    let other = bundle(dir.path(), "other", &binary(77, 300_000));
    let bytes = std::fs::read(&other).expect("read");
    let signature = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(None, &pair.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string(),
    );
    let verified =
        tauri_updater_delta_core::verify_artifact(bytes, &signature, &w.pubkey).expect("verify");
    cache
        .stage_pending("1.0.0", &verified, &signature)
        .expect("stage");
    cache.reconcile("1.0.0").expect("promote");

    let source = plan(&w, Some(&cache), &dir.path().join("work"));
    let reason = assert_fell_back_from_tar(&source);
    assert!(
        !w.server.fetched(TAR_PATCH_URL),
        "a wrong base must be caught before the patch is downloaded"
    );

    // *Which* gate caught it, not merely that something did. Three gates would
    // all refuse this artifact eventually — the recorded digests, the expanded
    // tar's digest, and the reconstruction — but only the first refuses it
    // before decompressing ten megabytes that cannot possibly be the right
    // base. Asserting the cheap one ran is the only way to know it still does.
    assert!(
        reason.contains("<cached base installer>"),
        "the recorded digest must reject a wrong base before it is expanded, \
         got: {reason}"
    );
}

#[test]
fn a_base_whose_recorded_tar_digest_is_wrong_is_caught_before_expansion() {
    // The second half of the same pre-filter. Here the compressed artifact is
    // the right one, so the installer-digest check passes, and the *tar* digest
    // the cache recorded does not match what the patch expects. Catching that
    // from the record avoids the expansion; the expansion would catch it too,
    // which is why this asserts which gate spoke.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let entry = w
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(&platform())
        .expect("platform");
    let tar_patch = entry
        .tar_layer
        .as_mut()
        .expect("tar layer")
        .patches
        .get_mut("1.0.0")
        .expect("patch");
    tar_patch.base_tar_blake3 = FileHash::of_bytes(b"a tar this base does not contain").to_hex();
    w.manifest_json = w.manifest.to_json().expect("serialise");

    let source = plan(&w, Some(&cache), &dir.path().join("work"));
    let reason = assert_fell_back_from_tar(&source);
    assert!(
        reason.contains("<cached base tar>"),
        "the recorded tar digest must reject the base before it is expanded, \
         got: {reason}"
    );
    assert!(!w.server.fetched(TAR_PATCH_URL));
}

#[test]
fn a_corrupt_tar_patch_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    w.server
        .replace(TAR_PATCH_URL, b"this is not a zstd frame".to_vec());
    let source = plan(&w, Some(&cache), &dir.path().join("work"));
    let _ = assert_fell_back_from_tar(&source);
    assert!(w.server.fetched(TAR_PATCH_URL));
}

#[test]
fn a_truncated_tar_patch_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let full = w.server.files.borrow()[TAR_PATCH_URL].clone();
    w.server
        .replace(TAR_PATCH_URL, full[..full.len() / 2].to_vec());
    let _ = assert_fell_back_from_tar(&plan(&w, Some(&cache), &dir.path().join("work")));
}

#[test]
fn a_patch_that_rebuilds_the_wrong_tar_falls_back() {
    // The digest gate on the reconstructed tar. The patch applies cleanly --
    // it is a real patch -- but between the wrong pair, so it produces a tar
    // that is not the one the manifest declares.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    // Point the manifest's declared target tar at something else, so a correct
    // reconstruction fails the gate.
    let entry = w
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(&platform())
        .expect("platform");
    entry
        .tar_layer
        .as_mut()
        .expect("tar layer")
        .target_tar_blake3 = FileHash::of_bytes(b"a different tar entirely").to_hex();
    w.manifest_json = w.manifest.to_json().expect("serialise");

    let source = plan(&w, Some(&cache), &dir.path().join("work"));
    let _ = assert_fell_back_from_tar(&source);
}

#[test]
fn a_recompression_that_misses_the_published_digest_falls_back() {
    // The gate that exists because the recipe is a recipe, not a guarantee.
    // The tar rebuilds correctly; the manifest's compressed-installer digest
    // says something else, which is what a client whose toolchain does not
    // reproduce the published bytes would observe.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let entry = w
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(&platform())
        .expect("platform");
    entry.target_installer_blake3 = FileHash::of_bytes(b"not what we will build").to_hex();
    w.manifest_json = w.manifest.to_json().expect("serialise");

    let source = plan(&w, Some(&cache), &dir.path().join("work"));
    let _ = assert_fell_back_from_tar(&source);
}

#[test]
fn an_unknown_representation_falls_back_without_touching_the_cache() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let entry = w
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(&platform())
        .expect("platform");
    entry.tar_layer.as_mut().expect("tar layer").representation = "app-tar-zstd-v2".to_owned();
    w.manifest_json = w.manifest.to_json().expect("serialise");

    let source = plan(&w, Some(&cache), &dir.path().join("work"));
    let UpdateSource::Full {
        reason, attempted, ..
    } = &source
    else {
        panic!("expected a full download, got {source:?}");
    };
    // Not "attempted": a representation this build does not implement means the
    // path was never entered, which is different from entering it and failing.
    assert!(!attempted.tar_delta);
    assert!(
        reason
            .as_ref()
            .map(|r| r.to_string().contains("app-tar-zstd-v2"))
            .unwrap_or(false),
        "the reason should name the representation: {reason:?}"
    );
    assert!(!w.server.fetched(TAR_PATCH_URL));
}

#[test]
fn an_unknown_recompression_recipe_falls_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let entry = w
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(&platform())
        .expect("platform");
    entry.tar_layer.as_mut().expect("tar layer").recompression = "tauri-app-tar-gz-v9".to_owned();
    w.manifest_json = w.manifest.to_json().expect("serialise");

    let UpdateSource::Full { reason, .. } = plan(&w, Some(&cache), &dir.path().join("work")) else {
        panic!("expected a full download");
    };
    assert!(reason
        .map(|r| r.to_string().contains("tauri-app-tar-gz-v9"))
        .unwrap_or(false));
}

// ---- what must fail closed, and what must not promote --------------------

#[test]
fn a_bad_signature_over_a_correctly_rebuilt_artifact_fails_closed() {
    // The one non-recoverable outcome (DECISIONS #11). The tar path rebuilds
    // the exact published bytes, so every digest gate passes; only the
    // signature disagrees, and that must abort rather than fall back.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    // A signature made by another key, over these exact bytes.
    let other = keypair();
    let bytes = w.released_bytes();
    let foreign = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(None, &other.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string(),
    );
    let identity = UpdateIdentity::new(
        "1.0.0",
        "1.0.1",
        "darwin",
        INSTALLER_URL,
        &foreign,
        &w.manifest_json,
    );

    let handoff = RecordingHandoff::default();
    let result = run_update(
        &identity,
        &Context {
            pubkey: &w.pubkey,
            base: None,
            cache: Some(&cache),
            app_id: "dev.example.testapp",
            work_dir: &dir.path().join("work"),
            limits: Limits::default(),
        },
        &w.server,
        &handoff,
    );

    // The manifest's own entries still carry the release signature, so this is
    // an identity mismatch before it is a signature failure -- either way it
    // must not install and must not fall back.
    assert!(result.is_err(), "got {result:?}");
    assert!(
        handoff.installed.borrow().is_empty(),
        "nothing may be installed when the signature does not hold"
    );
    assert!(
        cache.state().expect("state").pending.is_none(),
        "and nothing may be staged"
    );
}

#[test]
fn a_failed_install_does_not_promote_the_staged_artifact() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let handoff = RecordingHandoff {
        fail: true,
        ..Default::default()
    };
    let result = run(&w, Some(&cache), &handoff, &dir.path().join("work"));
    assert!(result.is_err(), "the installer refused, so the run failed");

    // The artifact was staged before the install was attempted, and staging is
    // not promotion. The next launch decides.
    let state = cache.state().expect("state");
    assert_eq!(state.active.expect("active").version, "1.0.0");
    assert_eq!(state.pending.expect("pending").version, "1.0.1");

    // The app relaunches as 1.0.0, because the install failed.
    assert_eq!(
        cache.reconcile("1.0.0").expect("reconcile"),
        Reconciliation::DiscardedPending {
            staged: "1.0.1".to_owned(),
            running: "1.0.0".to_owned(),
        }
    );
    let base = cache.active(&w.pubkey).expect("active").expect("base");
    assert_eq!(
        base.entry.version, "1.0.0",
        "a failed install must leave the base exactly as it was"
    );
}

#[test]
fn two_updates_running_at_once_do_not_produce_an_incorrect_active() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);
    let work = dir.path().join("work");

    let first = RecordingHandoff::default();
    let second = RecordingHandoff::default();
    let a = run(&w, Some(&cache), &first, &work).expect("first update");
    let b = run(&w, Some(&cache), &second, &work).expect("second update");

    assert_eq!(a.path_name(), "tar-delta");
    assert_eq!(b.path_name(), "tar-delta");
    assert_eq!(first.installed.borrow()[0], w.released_bytes());
    assert_eq!(second.installed.borrow()[0], w.released_bytes());

    let state = cache.state().expect("state");
    assert_eq!(state.active.expect("active").version, "1.0.0");
    assert_eq!(state.pending.expect("pending").version, "1.0.1");
    cache.reconcile("1.0.1").expect("reconcile");
    assert_eq!(
        cache
            .active(&w.pubkey)
            .expect("active")
            .expect("base")
            .artifact
            .as_bytes(),
        &w.released_bytes()[..]
    );
}

// ---- local resource ceilings on the tar pipeline -------------------------
//
// Gate B gave the flow one dial, `max_target_bytes`, and checked it against the
// compressed installer. The tar layer then put a four-stage pipeline in front of
// that number, every stage sized by the same unauthenticated document. These
// tests are about the stage that had no local ceiling above it at all.
//
// The shape they all share: the manifest asks for something unreasonable, the
// tar path is *attempted* and refused, and the update still completes as a full
// download. Refusing to blow up the machine is not the same as refusing to
// update.

/// The reason string a size refusal produces, for tests that assert on which
/// gate spoke rather than merely that something did.
fn assert_refused_for_size(source: &UpdateSource) -> String {
    let reason = assert_fell_back_from_tar(source);
    assert!(
        reason.contains("declares") && reason.contains("local limit"),
        "expected a declared-size refusal, got: {reason}"
    );
    reason
}

#[test]
fn an_absurd_declared_target_tar_is_refused_before_anything_is_touched() {
    // The gap this closes. `target_tar_size` becomes zstd's window and output
    // bound, so before there was a local ceiling above it a manifest could ask
    // this client to reserve and write half a terabyte -- and every digest and
    // signature check happens far too late to matter.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    edit_tar_layer(&mut w, |layer| {
        layer.target_tar_size = 500 * 1024 * 1024 * 1024;
    });

    let source = plan(&w, Some(&cache), &dir.path().join("work"));
    assert_refused_for_size(&source);

    // Nothing was downloaded and nothing was expanded: the check is a
    // comparison against a local constant, so it costs one branch.
    assert!(
        !w.server.fetched(TAR_PATCH_URL),
        "an absurd declaration must be refused before the patch is fetched"
    );
}

#[test]
fn an_absurd_declared_base_tar_is_refused_before_the_cache_is_expanded() {
    // The other direction: `base_tar_size` sizes the expansion of a blob we
    // already hold. Still a server-supplied number, so still capped locally.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let mut w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let entry = w
        .manifest
        .delta
        .as_mut()
        .expect("delta")
        .platforms
        .get_mut(&platform())
        .expect("platform");
    entry
        .tar_layer
        .as_mut()
        .expect("tar layer")
        .patches
        .get_mut("1.0.0")
        .expect("patch")
        .base_tar_size = 900 * 1024 * 1024 * 1024;
    w.manifest_json = w.manifest.to_json().expect("serialise");

    assert_refused_for_size(&plan(&w, Some(&cache), &dir.path().join("work")));
    assert!(!w.server.fetched(TAR_PATCH_URL));
}

#[test]
fn the_local_ceiling_binds_even_when_the_manifest_is_honest() {
    // An absurdity filter and a policy ceiling are different things. This
    // manifest is entirely truthful -- the tar really is that size -- and the
    // host has simply said it will not reconstruct one that big. The refusal
    // must come from the host's number, not from the manifest disagreeing with
    // itself, or the cap is decorative on any release whose metadata is
    // internally consistent.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let honest_size = w.manifest.delta.as_ref().expect("delta").platforms[&platform()]
        .tar_layer
        .as_ref()
        .expect("tar layer")
        .target_tar_size;
    assert!(honest_size > 1024, "fixture sanity: the tar has real size");

    let source = plan_with_limits(
        &w,
        Some(&cache),
        &dir.path().join("work"),
        Limits {
            max_target_bytes: u64::MAX,
            max_tar_bytes: honest_size - 1,
        },
    );
    assert_refused_for_size(&source);
    assert!(!w.server.fetched(TAR_PATCH_URL));
}

#[test]
fn a_size_exactly_at_the_ceiling_is_still_allowed() {
    // The boundary, in the direction that costs a user a working update if it
    // is wrong. An off-by-one here silently disables the tar path for anyone
    // whose artifact happens to sit on the limit.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);
    let cache = open_cache(&dir.path().join("cache"), &w.pubkey);
    seed_active(&cache, &w, &pair);

    let layer = w.manifest.delta.as_ref().expect("delta").platforms[&platform()]
        .tar_layer
        .as_ref()
        .expect("tar layer");
    let ceiling = layer
        .target_tar_size
        .max(layer.patches["1.0.0"].base_tar_size);

    let source = plan_with_limits(
        &w,
        Some(&cache),
        &dir.path().join("work"),
        Limits {
            max_target_bytes: Limits::default().max_target_bytes,
            max_tar_bytes: ceiling,
        },
    );
    assert_eq!(
        source.path_name(),
        "tar-delta",
        "a tar exactly at the ceiling must still be reconstructed"
    );
}

#[test]
fn a_cached_blob_that_expands_past_the_cache_ceiling_falls_back() {
    // A decompression bomb reaching the pipeline through the cache rather than
    // over the network. gzip's ratio is unbounded, so the artifact that
    // describes a huge tar is itself small enough to have passed the blob
    // ceiling on the way in.
    //
    // The cache's own dial catches this one, which is the point: the ceilings
    // are independent, so a host that tightened one has not accidentally
    // loosened another.
    let dir = tempfile::tempdir().expect("temp dir");
    let pair = keypair();
    let w = world(dir.path(), &pair);

    let tight = ArtifactCache::open(
        dir.path().join("cache"),
        namespace(&w.pubkey),
        CacheLimits {
            max_blob_bytes: 64 * 1024 * 1024,
            // Far below the fixture's real tar.
            max_tar_bytes: 1024,
            blob_grace: std::time::Duration::from_secs(0),
        },
    )
    .expect("open cache");

    // Staging expands the artifact to record its tar digest, so the ceiling is
    // enforced on the way in as well as on the way out.
    let bytes = std::fs::read(&w.old).expect("read old artifact");
    let signature = base64::engine::general_purpose::STANDARD.encode(
        minisign::sign(None, &pair.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string(),
    );
    let verified = tauri_updater_delta_core::verify_artifact(bytes, &signature, &w.pubkey)
        .expect("the seeded base must verify");
    let staged = tight.stage_pending("1.0.0", &verified, &signature);
    assert!(
        staged.is_err(),
        "a blob that expands past the ceiling must not be cached at all"
    );

    // And with nothing cached, the update still happens.
    let handoff = RecordingHandoff::default();
    let outcome = run(&w, Some(&tight), &handoff, &dir.path().join("work")).expect("update");
    assert_eq!(outcome.path_name(), "full");
    assert_eq!(handoff.installed.borrow()[0], w.released_bytes());
}

#[test]
fn a_malformed_tar_is_rejected_by_a_linear_walk_not_an_extraction() {
    // Recompression is the only stage that reads tar *structure*, and it does so
    // to replay write boundaries -- never to extract entries onto disk. This
    // feeds it a header claiming an entry far larger than the archive, which is
    // the shape that turns a naive extractor into an out-of-memory or a
    // path-traversal bug.
    use tauri_updater_delta_core::recompress::recompress_app_tar_gz;

    let dir = tempfile::tempdir().expect("temp dir");
    let bad = dir.path().join("bad.tar");
    let out = dir.path().join("out.tar.gz");

    // A plausible 512-byte tar header whose size field claims 8 GB, followed by
    // nothing.
    let mut header = [0u8; 512];
    header[..9].copy_from_slice(b"evil.bin\0");
    // Size field at offset 124, 12 bytes, octal, NUL-terminated.
    header[124..135].copy_from_slice(b"77777777777");
    header[156] = b'0';
    std::fs::write(&bad, header).expect("write malformed tar");

    let err = recompress_app_tar_gz(&bad, &out).expect_err("a malformed tar must be refused");
    let text = err.to_string();
    assert!(
        text.contains("malformed tar"),
        "expected a malformed-tar refusal, got: {text}"
    );

    // The refusal is a bounded read, so nothing resembling an extracted tree
    // appears next to it.
    let siblings: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert!(
        siblings.len() <= 2,
        "a malformed tar must not produce files beyond its own output: {siblings:?}"
    );
}
