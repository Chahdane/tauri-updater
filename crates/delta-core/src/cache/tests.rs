//! The cache state machine, exercised against real signed artifacts.
//!
//! Everything here uses a genuine minisign keypair and a genuine `.app.tar.gz`
//! built by `tar::Builder` into a `GzEncoder`, because the properties under test
//! are exactly the ones a stub would paper over: that verification is re-run on
//! every reuse, and that a key rotation invalidates the cache.

use super::*;
use crate::signature::verify_artifact;

/// A signing key and the public key that goes with it, in Tauri's encodings.
struct Key {
    secret: minisign::SecretKey,
    public: String,
    fingerprint: String,
}

fn key() -> Key {
    use base64::Engine as _;
    let pair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new())).expect("keypair");
    let public = base64::engine::general_purpose::STANDARD
        .encode(pair.pk.to_box().expect("box pk").into_string());
    let fingerprint = FileHash::of_bytes(public.as_bytes()).to_hex();
    Key {
        secret: pair.sk,
        public,
        fingerprint,
    }
}

impl Key {
    fn sign(&self, bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(
            minisign::sign(None, &self.secret, bytes, None, None)
                .expect("sign")
                .into_string(),
        )
    }

    /// A real `.app.tar.gz` for `version`, signed, as a verified artifact.
    fn artifact(&self, version: &str) -> (VerifiedArtifact, String) {
        use std::io::Write as _;

        let payload: Vec<u8> = format!("main binary for {version}")
            .bytes()
            .cycle()
            .take(20_000)
            .collect();

        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "Bundle.app/Contents/MacOS/app", &payload[..])
            .expect("append");
        let bytes = builder
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish gzip");

        let _ = std::io::stdout().flush();
        let signature = self.sign(&bytes);
        let artifact = verify_artifact(bytes, &signature, &self.public).expect("must verify");
        (artifact, signature)
    }
}

fn namespace(fingerprint: &str) -> Namespace {
    Namespace {
        bundle_id: "com.example.delta".to_owned(),
        platform: "darwin-aarch64".to_owned(),
        arch: "aarch64".to_owned(),
        pubkey_fingerprint: fingerprint.to_owned(),
        representation: crate::manifest::REPRESENTATION_APP_TAR_GZ_V1.to_owned(),
        recompression: crate::manifest::RECOMPRESSION_TAURI_APP_TAR_GZ_V1.to_owned(),
    }
}

fn cache(root: &Path, key: &Key) -> ArtifactCache {
    ArtifactCache::open(root, namespace(&key.fingerprint), CacheLimits::default()).expect("open")
}

/// A cache that collects unreferenced blobs immediately, for the tests that are
/// about collection rather than about the race it guards.
fn eager_cache(root: &Path, key: &Key) -> ArtifactCache {
    ArtifactCache::open(
        root,
        namespace(&key.fingerprint),
        CacheLimits {
            blob_grace: std::time::Duration::ZERO,
            ..CacheLimits::default()
        },
    )
    .expect("open")
}

// ---- the happy lifecycle ------------------------------------------------

#[test]
fn an_empty_cache_offers_no_base() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);

    assert_eq!(
        cache.reconcile("1.0.0").expect("reconcile"),
        Reconciliation::NothingStaged
    );
    assert!(cache.active(&k.public).expect("active").is_none());
}

#[test]
fn a_staged_artifact_is_promoted_only_once_the_app_runs_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);
    let (artifact, signature) = k.artifact("1.0.1");

    cache
        .stage_pending("1.0.1", &artifact, &signature)
        .expect("stage");

    // Still running the old version: nothing may be promoted yet, even though
    // the artifact is fully verified and sitting in the cache.
    let state = cache.state().expect("state");
    assert!(state.active.is_none());
    assert_eq!(state.pending.expect("pending").version, "1.0.1");
    assert!(
        cache.active(&k.public).expect("active").is_none(),
        "a pending artifact must never be offered as a base"
    );

    // The app comes back up as 1.0.1. Now it is the base.
    assert_eq!(
        cache.reconcile("1.0.1").expect("reconcile"),
        Reconciliation::Promoted {
            version: "1.0.1".to_owned()
        }
    );
    let base = cache
        .active(&k.public)
        .expect("active")
        .expect("there must be an active base");
    assert_eq!(base.entry.version, "1.0.1");
    assert_eq!(base.artifact.as_bytes(), artifact.as_bytes());
    assert!(cache.state().expect("state").pending.is_none());
}

#[test]
fn an_update_over_an_existing_base_keeps_the_old_one_until_the_new_one_runs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);
    let (old, old_sig) = k.artifact("1.0.1");
    let (new, new_sig) = k.artifact("1.0.2");

    cache.stage_pending("1.0.1", &old, &old_sig).expect("stage");
    cache.reconcile("1.0.1").expect("promote");
    cache.stage_pending("1.0.2", &new, &new_sig).expect("stage");

    // ACTIVE(1.0.1) + PENDING(1.0.2): the base offered is still 1.0.1, because
    // that is still what is running.
    let base = cache.active(&k.public).expect("active").expect("base");
    assert_eq!(base.entry.version, "1.0.1");
    assert_eq!(base.artifact.as_bytes(), old.as_bytes());

    cache.reconcile("1.0.2").expect("promote");
    let base = cache.active(&k.public).expect("active").expect("base");
    assert_eq!(base.entry.version, "1.0.2");
    assert_eq!(base.artifact.as_bytes(), new.as_bytes());
}

#[test]
fn the_cached_tar_digest_matches_the_artifact_it_describes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);
    let (artifact, signature) = k.artifact("1.0.1");

    let entry = cache
        .stage_pending("1.0.1", &artifact, &signature)
        .expect("stage");

    let out = dir.path().join("expanded.tar");
    cache
        .expand_to_tar(&artifact, &out, &entry.tar_blake3, entry.tar_size)
        .expect("the recorded tar digest must describe this artifact");
    assert_eq!(std::fs::metadata(&out).expect("stat").len(), entry.tar_size);
}

// ---- everything that must not promote -----------------------------------

#[test]
fn a_failed_install_leaves_the_previous_base_intact() {
    // No install-result signal exists, and that is the design: the next launch
    // is still running the old version, so the staged artifact is discarded by
    // the same rule that handles every other non-promotion.
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);
    let (old, old_sig) = k.artifact("1.0.1");
    let (new, new_sig) = k.artifact("1.0.2");

    cache.stage_pending("1.0.1", &old, &old_sig).expect("stage");
    cache.reconcile("1.0.1").expect("promote");
    cache.stage_pending("1.0.2", &new, &new_sig).expect("stage");

    // The install failed; the app relaunches as 1.0.1.
    assert_eq!(
        cache.reconcile("1.0.1").expect("reconcile"),
        Reconciliation::DiscardedPending {
            staged: "1.0.2".to_owned(),
            running: "1.0.1".to_owned(),
        }
    );

    let state = cache.state().expect("state");
    assert!(state.pending.is_none(), "the staged artifact must be gone");
    assert_eq!(
        state.active.expect("active").version,
        "1.0.1",
        "a failed install must not disturb the base"
    );
    let base = cache.active(&k.public).expect("active").expect("base");
    assert_eq!(base.artifact.as_bytes(), old.as_bytes());
}

#[test]
fn a_stale_pending_from_a_version_nobody_is_running_is_discarded() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);
    let (artifact, signature) = k.artifact("1.0.2");
    cache
        .stage_pending("1.0.2", &artifact, &signature)
        .expect("stage");

    // The user installed 1.0.3 from a download page instead.
    assert_eq!(
        cache.reconcile("1.0.3").expect("reconcile"),
        Reconciliation::DiscardedPending {
            staged: "1.0.2".to_owned(),
            running: "1.0.3".to_owned(),
        }
    );
    assert!(cache.active(&k.public).expect("active").is_none());
}

#[test]
fn rotating_the_signing_key_empties_the_cache() {
    let dir = tempfile::tempdir().expect("temp dir");
    let old_key = key();
    let new_key = key();

    {
        let cache = cache(dir.path(), &old_key);
        let (artifact, signature) = old_key.artifact("1.0.1");
        cache
            .stage_pending("1.0.1", &artifact, &signature)
            .expect("stage");
        cache.reconcile("1.0.1").expect("promote");
        assert!(cache.active(&old_key.public).expect("active").is_some());
    }

    // Same directory, different key. The artifacts in it are genuine — they are
    // simply not signed by the key this build now trusts.
    let cache = cache(dir.path(), &new_key);
    assert_eq!(
        cache.reconcile("1.0.1").expect("reconcile"),
        Reconciliation::NamespaceReset
    );
    assert!(
        cache.active(&new_key.public).expect("active").is_none(),
        "a rotated key must not leave the old base reusable"
    );
    assert_eq!(
        cache.blobs().size_on_disk().expect("size"),
        0,
        "and the artifacts themselves must go, not just the pointer to them"
    );
}

#[test]
fn a_cache_from_another_platform_is_a_miss_not_a_repair() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    {
        let cache = cache(dir.path(), &k);
        let (artifact, signature) = k.artifact("1.0.1");
        cache
            .stage_pending("1.0.1", &artifact, &signature)
            .expect("stage");
        cache.reconcile("1.0.1").expect("promote");
    }

    let mut foreign = namespace(&k.fingerprint);
    foreign.arch = "x86_64".to_owned();
    foreign.platform = "darwin-x86_64".to_owned();
    let cache = ArtifactCache::open(dir.path(), foreign, CacheLimits::default()).expect("open");
    assert_eq!(
        cache.reconcile("1.0.1").expect("reconcile"),
        Reconciliation::NamespaceReset
    );
    assert!(cache.active(&k.public).expect("active").is_none());
}

#[test]
fn a_cache_written_for_another_recompression_recipe_is_not_reused() {
    // The identifiers are in the namespace precisely so that a client which
    // changes what it can rebuild does not go on using a base recorded under
    // the old capability.
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    {
        let cache = cache(dir.path(), &k);
        let (artifact, signature) = k.artifact("1.0.1");
        cache
            .stage_pending("1.0.1", &artifact, &signature)
            .expect("stage");
        cache.reconcile("1.0.1").expect("promote");
    }

    let mut future = namespace(&k.fingerprint);
    future.recompression = "tauri-app-tar-gz-v2".to_owned();
    let cache = ArtifactCache::open(dir.path(), future, CacheLimits::default()).expect("open");
    assert_eq!(
        cache.reconcile("1.0.1").expect("reconcile"),
        Reconciliation::NamespaceReset
    );
}

#[test]
fn a_corrupt_blob_is_a_cache_miss_and_leaves_the_state_alone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);
    let (artifact, signature) = k.artifact("1.0.1");
    let entry = cache
        .stage_pending("1.0.1", &artifact, &signature)
        .expect("stage");
    cache.reconcile("1.0.1").expect("promote");

    let blob = dir
        .path()
        .join("blobs")
        .join(format!("{}.tar.gz", entry.compressed_blake3));
    let mut bytes = std::fs::read(&blob).expect("read blob");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&blob, &bytes).expect("corrupt");

    let err = cache
        .active(&k.public)
        .expect_err("a corrupt base must not be offered");
    assert!(matches!(err, Error::ChecksumMismatch { .. }), "{err}");

    // And the state is untouched: the cache reports a miss, it does not
    // rewrite itself in response to one.
    assert_eq!(
        cache
            .state()
            .expect("state")
            .active
            .expect("active")
            .version,
        "1.0.1"
    );
}

#[test]
fn a_tar_that_does_not_match_the_manifests_declaration_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);
    let (artifact, signature) = k.artifact("1.0.1");
    let entry = cache
        .stage_pending("1.0.1", &artifact, &signature)
        .expect("stage");

    let out = dir.path().join("wrong.tar");
    let wrong = FileHash::of_bytes(b"a different tar entirely").to_hex();
    let err = cache
        .expand_to_tar(&artifact, &out, &wrong, entry.tar_size)
        .expect_err("a base tar digest mismatch must be refused");
    assert!(matches!(err, Error::ChecksumMismatch { .. }), "{err}");
    assert!(!out.exists(), "and the wrong tar must not be left behind");

    let err = cache
        .expand_to_tar(&artifact, &out, &entry.tar_blake3, entry.tar_size + 1)
        .expect_err("a base tar size mismatch must be refused");
    assert!(matches!(err, Error::UnexpectedOutputSize { .. }), "{err}");
}

// ---- crash and contention ------------------------------------------------

#[test]
fn an_orphan_blob_from_a_crashed_update_is_collected() {
    // A process that stored a blob and died before staging it leaves a file no
    // entry names. It must not accumulate, and it must not be mistaken for a
    // base.
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = eager_cache(dir.path(), &k);
    let (kept, kept_sig) = k.artifact("1.0.1");
    let (orphan, _) = k.artifact("9.9.9");

    cache
        .stage_pending("1.0.1", &kept, &kept_sig)
        .expect("stage");
    let orphan_digest = cache.blobs().put(&orphan).expect("put orphan");
    assert!(cache.blobs().contains(&orphan_digest.to_hex()));

    cache.reconcile("1.0.1").expect("reconcile");
    assert!(
        !cache.blobs().contains(&orphan_digest.to_hex()),
        "a blob no entry names must be collected"
    );
    assert!(
        cache.active(&k.public).expect("active").is_some(),
        "and collection must not take the live base with it"
    );
}

#[test]
fn a_scratch_directory_does_not_outlive_its_update() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let cache = cache(dir.path(), &k);
    let tmp = dir.path().join("tmp");

    {
        let scratch = cache.scratch().expect("scratch");
        std::fs::write(scratch.path().join("work"), b"in progress").expect("write");
        assert_eq!(std::fs::read_dir(&tmp).expect("read").count(), 1);
    }
    assert_eq!(
        std::fs::read_dir(&tmp).expect("read").count(),
        0,
        "scratch must be removed with the value that owns it"
    );
}

#[test]
fn a_state_file_from_a_future_format_is_not_interpreted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let k = key();
    let mut state = CacheState::empty(namespace(&k.fingerprint));
    state.format_version = CACHE_FORMAT_VERSION + 1;
    state.active = Some(CacheEntry {
        version: "9.9.9".to_owned(),
        compressed_blake3: "00".repeat(32),
        compressed_size: 1,
        tar_blake3: "11".repeat(32),
        tar_size: 1,
        signature: "sig".to_owned(),
    });
    std::fs::write(
        dir.path().join("state.0.json"),
        serde_json::to_string(&state).expect("serialize"),
    )
    .expect("write");

    let cache = cache(dir.path(), &k);
    assert!(
        cache.active(&k.public).expect("active").is_none(),
        "a state written by a future format must not be read as a base"
    );
}

#[test]
fn concurrent_transitions_never_produce_an_incorrect_active() {
    // Several processes reconciling and staging at once. The invariant is not
    // that every transition survives — a discard racing a stage is allowed to
    // lose — but that ACTIVE is only ever a version that was actually staged
    // and then observed running, and that the state never becomes unreadable.
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("temp dir");
    let k = Arc::new(key());
    let cache = Arc::new(cache(dir.path(), &k));

    let (first, first_sig) = k.artifact("1.0.0");
    cache
        .stage_pending("1.0.0", &first, &first_sig)
        .expect("stage");
    cache.reconcile("1.0.0").expect("promote");

    let artifacts: Vec<_> = (1..=4)
        .map(|i| {
            let version = format!("1.0.{i}");
            let (artifact, signature) = k.artifact(&version);
            (version, artifact, signature)
        })
        .collect();
    let artifacts = Arc::new(artifacts);

    std::thread::scope(|scope| {
        for worker in 0..4 {
            let cache = Arc::clone(&cache);
            let artifacts = Arc::clone(&artifacts);
            scope.spawn(move || {
                for round in 0..5 {
                    let (version, artifact, signature) = &artifacts[worker];
                    cache
                        .stage_pending(version, artifact, signature)
                        .expect("stage must survive contention");
                    if round % 2 == 0 {
                        cache
                            .reconcile(version)
                            .expect("reconcile must survive contention");
                    }
                }
            });
        }
    });

    let state = cache.state().expect("state must still be readable");
    let active = state.active.expect("an active entry must survive");
    assert!(
        active.version == "1.0.0" || artifacts.iter().any(|(v, _, _)| *v == active.version),
        "ACTIVE names a version nobody staged: {}",
        active.version
    );

    // Whatever won, it must still be a usable, verifiable base.
    let base = cache
        .active(&k.public)
        .expect("active")
        .expect("the surviving base must be readable");
    assert_eq!(base.entry.version, active.version);
    assert_eq!(
        FileHash::of_bytes(base.artifact.as_bytes()).to_hex(),
        active.compressed_blake3,
        "the blob ACTIVE names must be the blob ACTIVE describes"
    );
}
