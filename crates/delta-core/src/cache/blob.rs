//! Content-addressed storage for verified installer artifacts.
//!
//! # Two invariants, both structural
//!
//! **Nothing unverified goes in.** [`BlobStore::put`] takes a
//! [`VerifiedArtifact`], which only `verify_artifact` can construct. There is no
//! `put_bytes`, no `put_file`, and no way to spell "cache these bytes I have not
//! checked" — the same move as the install handoff, applied to the other end of
//! the artifact's life.
//!
//! **Nothing trusted comes out.** [`BlobStore::get`] re-reads the file, re-hashes
//! it, and re-verifies its signature against the *currently configured* public
//! key before returning anything. Being in the cache establishes nothing: the
//! artifact was verified when it was written, on a machine where other processes,
//! backup tools and the user all have write access to the directory it sits in.
//!
//! # Why filenames are derived, never supplied
//!
//! A blob's name is the BLAKE3 of its contents, computed here. No manifest field,
//! no URL component and no version string reaches the path, so a manifest cannot
//! address a file outside the cache directory however it is spelled. The
//! alternative — naming blobs after something the server said — makes path
//! traversal a matter of quoting discipline. This makes it unrepresentable.
//!
//! # Why blobs need no locking
//!
//! Content-addressing means two processes computing the same artifact write the
//! same bytes. A race over a blob costs duplicated work and nothing else, which
//! is why the mutable half of the cache — which entry is ACTIVE and which is
//! PENDING — lives behind a compare-and-set in [`super::state`] instead, and this
//! half does not.
//!
//! Publication still uses `hard_link` rather than `rename`: a blob that already
//! exists is already correct, so the right answer to a collision is to keep what
//! is there, and `rename` would instead replace a complete file with another
//! complete file for no reason. `AlreadyExists` here is success.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::signature::{verify_artifact, VerifiedArtifact};
use crate::{Error, FileHash, Result};

/// Extension every blob carries. Cosmetic — the digest is the identity.
const BLOB_EXTENSION: &str = "tar.gz";

/// Immutable, content-addressed artifact storage.
pub struct BlobStore {
    dir: PathBuf,
}

impl BlobStore {
    /// Open (or create) a store rooted at `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| Error::io("create", &dir, e))?;
        Ok(Self { dir })
    }

    /// The directory blobs live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where a blob with this digest would live.
    ///
    /// Private on purpose. Handing out paths invites a caller to read one
    /// without going through [`get`](BlobStore::get), which is where the
    /// re-verification is.
    fn path_for(&self, digest_hex: &str) -> PathBuf {
        self.dir.join(format!("{digest_hex}.{BLOB_EXTENSION}"))
    }

    /// Store a verified artifact, returning its digest.
    ///
    /// Idempotent: storing bytes already present is a no-op that reports
    /// success, because the file that is there has the same digest and is
    /// therefore the same file.
    ///
    /// The write is staged under a unique temporary name, flushed and
    /// `sync_all`ed, and only then linked to its content address — so a crash
    /// can leave a stray temporary file but never a partial blob under a name
    /// that claims a digest it does not have.
    pub fn put(&self, artifact: &VerifiedArtifact) -> Result<FileHash> {
        let digest = FileHash::of_bytes(artifact.as_bytes());
        let target = self.path_for(&digest.to_hex());

        let temp = self.dir.join(format!(
            "{}.{}.{:?}.part",
            digest.to_hex(),
            std::process::id(),
            std::thread::current().id()
        ));

        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(artifact.as_bytes())?;
            file.flush()?;
            file.sync_all()
        };
        write().map_err(|e| Error::io("write", &temp, e))?;

        let linked = std::fs::hard_link(&temp, &target);
        let _ = std::fs::remove_file(&temp);
        match linked {
            Ok(()) => Ok(digest),
            // Already stored. Content addressing makes that the same bytes, so
            // there is nothing to reconcile and nothing to overwrite.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(digest),
            Err(e) => Err(Error::io("publish", &target, e)),
        }
    }

    /// Whether a blob with this digest is present.
    ///
    /// A cheap pre-check only. It says nothing about the file being intact, so
    /// it is never a substitute for [`get`](BlobStore::get).
    pub fn contains(&self, digest_hex: &str) -> bool {
        self.path_for(digest_hex).is_file()
    }

    /// Read a blob back, re-verifying everything about it.
    ///
    /// In order: the entry is a regular file (not a symlink, directory or
    /// device), its size matches what the caller expects and is within
    /// `max_bytes`, its contents hash to `digest_hex`, and its signature
    /// verifies against `pubkey`.
    ///
    /// The bytes are read **once** and every check runs against that one
    /// allocation, which is also what is returned. Verifying a path and then
    /// reopening it would leave a window in which the file could be replaced
    /// between the check and the use; there is no such window here, for the
    /// same reason [`VerifiedArtifact`] holds bytes rather than a path.
    ///
    /// Every failure means **cache miss**. None of them means "repair it": a
    /// blob that fails any of these checks is not a damaged copy of something
    /// trustworthy, it is an untrusted file that happens to be in the cache
    /// directory.
    pub fn get(
        &self,
        digest_hex: &str,
        expected_size: u64,
        signature_b64: &str,
        pubkey_b64: &str,
        max_bytes: u64,
    ) -> Result<VerifiedArtifact> {
        let expected = FileHash::from_hex(digest_hex)?;
        let path = self.path_for(digest_hex);

        // Refuse anything that is not a regular file *before* opening it, so a
        // symlink planted in the cache directory is rejected by kind rather
        // than followed and judged by its target.
        let meta = std::fs::symlink_metadata(&path).map_err(|e| Error::io("stat", &path, e))?;
        if !meta.is_file() {
            return Err(Error::Manifest(format!(
                "cache entry {} is not a regular file",
                path.display()
            )));
        }

        if expected_size > max_bytes {
            return Err(Error::DeclaredSizeTooLarge {
                declared: expected_size,
                limit: max_bytes,
            });
        }

        let mut file = std::fs::File::open(&path).map_err(|e| Error::io("read", &path, e))?;

        // And again on the open handle, which is bound to the inode rather than
        // to the name. Between the check above and this one the path could have
        // been re-pointed; this is what actually got opened.
        let opened = file.metadata().map_err(|e| Error::io("stat", &path, e))?;
        if !opened.is_file() {
            return Err(Error::Manifest(format!(
                "cache entry {} is not a regular file",
                path.display()
            )));
        }
        if opened.len() != expected_size {
            return Err(Error::UnexpectedOutputSize {
                expected: expected_size,
                actual: opened.len(),
            });
        }

        // Bounded by one byte more than expected, so a file that grew between
        // the stat and the read is detected rather than silently truncated.
        let mut bytes = Vec::with_capacity(expected_size as usize);
        let read = (&mut file)
            .take(expected_size + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::io("read", &path, e))?;
        if read as u64 != expected_size {
            return Err(Error::UnexpectedOutputSize {
                expected: expected_size,
                actual: read as u64,
            });
        }

        let actual = FileHash::of_bytes(&bytes);
        if actual != expected {
            return Err(Error::ChecksumMismatch {
                path,
                expected: expected.to_hex(),
                actual: actual.to_hex(),
            });
        }

        // Against the key configured *now*. A key rotation must not leave
        // artifacts signed by a retired key reusable, and the namespace binding
        // in `super::state` is a cheap pre-filter for that, not the check.
        verify_artifact(bytes, signature_b64, pubkey_b64)
    }

    /// Delete every blob whose digest is not in `keep` and which is at least
    /// `grace` old.
    ///
    /// Returns how many were removed. Files that do not look like blobs are
    /// left alone rather than tidied away — this owns its own naming scheme and
    /// nothing else's.
    ///
    /// # Why the age floor
    ///
    /// Staging writes the blob and *then* commits the entry naming it. In
    /// between, the blob is referenced by nothing and a concurrent collection is
    /// entitled to delete it, leaving the stager to commit a reference to a file
    /// that is gone. `grace` makes that window unreachable without serialising
    /// updates behind a lock — see [`super::CacheLimits::blob_grace`].
    ///
    /// A blob whose age cannot be determined is treated as young and kept.
    /// Failing to collect costs disk; collecting something live costs a
    /// download.
    pub fn retain(&self, keep: &[&str], grace: std::time::Duration) -> Result<usize> {
        let entries = std::fs::read_dir(&self.dir).map_err(|e| Error::io("read", &self.dir, e))?;
        let now = std::time::SystemTime::now();
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(digest) = name.strip_suffix(&format!(".{BLOB_EXTENSION}")) else {
                continue;
            };
            if FileHash::from_hex(digest).is_err() {
                continue;
            }
            if keep.contains(&digest) {
                continue;
            }
            if !grace.is_zero() {
                let age = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| now.duration_since(t).ok());
                match age {
                    Some(age) if age >= grace => {}
                    _ => continue,
                }
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Total bytes held, for reporting.
    pub fn size_on_disk(&self) -> Result<u64> {
        let entries = std::fs::read_dir(&self.dir).map_err(|e| Error::io("read", &self.dir, e))?;
        Ok(entries
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real keypair and a real signature over `bytes`, so the tests exercise
    /// the verification path rather than a stub of it.
    fn signed(bytes: &[u8]) -> (VerifiedArtifact, String, String) {
        use base64::Engine as _;
        let pair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new()))
            .expect("generate keypair");
        let signature = base64::engine::general_purpose::STANDARD.encode(
            minisign::sign(None, &pair.sk, bytes, None, None)
                .expect("sign")
                .into_string(),
        );
        let pubkey = base64::engine::general_purpose::STANDARD
            .encode(pair.pk.to_box().expect("box pk").into_string());
        let artifact =
            verify_artifact(bytes.to_vec(), &signature, &pubkey).expect("fixture must verify");
        (artifact, signature, pubkey)
    }

    const HUGE: u64 = 1 << 30;

    #[test]
    fn a_stored_artifact_comes_back_verified() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let (artifact, signature, pubkey) = signed(b"the released installer");

        let digest = store.put(&artifact).expect("put");
        assert!(store.contains(&digest.to_hex()));

        let back = store
            .get(
                &digest.to_hex(),
                artifact.len() as u64,
                &signature,
                &pubkey,
                HUGE,
            )
            .expect("get");
        assert_eq!(back.as_bytes(), artifact.as_bytes());
    }

    #[test]
    fn the_filename_is_the_digest_and_nothing_else() {
        // The path-traversal property: no input reaches the path except bytes
        // this store hashed itself.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let (artifact, _, _) = signed(b"payload");
        let digest = store.put(&artifact).expect("put");

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![format!("{}.tar.gz", digest.to_hex())]);
    }

    #[test]
    fn storing_the_same_artifact_twice_is_a_no_op() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let (artifact, signature, pubkey) = signed(b"the released installer");

        let first = store.put(&artifact).expect("first put");
        let second = store.put(&artifact).expect("second put");
        assert_eq!(first, second);
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("read dir").count(),
            1,
            "content addressing means one file, not two"
        );
        store
            .get(
                &first.to_hex(),
                artifact.len() as u64,
                &signature,
                &pubkey,
                HUGE,
            )
            .expect("still readable");
    }

    #[test]
    fn a_corrupted_blob_is_a_cache_miss() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let (artifact, signature, pubkey) = signed(b"the released installer");
        let digest = store.put(&artifact).expect("put");

        // Same length, different bytes — so only the digest can catch it.
        let path = dir.path().join(format!("{}.tar.gz", digest.to_hex()));
        let mut bytes = std::fs::read(&path).expect("read");
        bytes[0] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("corrupt");

        let err = store
            .get(
                &digest.to_hex(),
                artifact.len() as u64,
                &signature,
                &pubkey,
                HUGE,
            )
            .expect_err("a corrupted blob must not be returned");
        assert!(matches!(err, Error::ChecksumMismatch { .. }), "{err}");
    }

    #[test]
    fn a_blob_of_the_wrong_length_is_refused_before_it_is_hashed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let (artifact, signature, pubkey) = signed(b"the released installer");
        let digest = store.put(&artifact).expect("put");

        let err = store
            .get(
                &digest.to_hex(),
                artifact.len() as u64 + 1,
                &signature,
                &pubkey,
                HUGE,
            )
            .expect_err("a size disagreement must be refused");
        assert!(matches!(err, Error::UnexpectedOutputSize { .. }), "{err}");
    }

    #[test]
    fn a_blob_over_the_local_ceiling_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let (artifact, signature, pubkey) = signed(b"the released installer");
        let digest = store.put(&artifact).expect("put");

        let err = store
            .get(
                &digest.to_hex(),
                artifact.len() as u64,
                &signature,
                &pubkey,
                4,
            )
            .expect_err("the host's ceiling must win");
        assert!(matches!(err, Error::DeclaredSizeTooLarge { .. }), "{err}");
    }

    #[test]
    fn a_blob_signed_by_a_retired_key_is_a_cache_miss() {
        // Key rotation. The bytes are intact and their digest is right; the
        // signature is simply not one the currently configured key made.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let (artifact, signature, _retired) = signed(b"the released installer");
        let (_, _, current_pubkey) = signed(b"some other release");
        let digest = store.put(&artifact).expect("put");

        let err = store
            .get(
                &digest.to_hex(),
                artifact.len() as u64,
                &signature,
                &current_pubkey,
                HUGE,
            )
            .expect_err("a retired key's signature must not be accepted");
        assert!(matches!(err, Error::Signature(_)), "{err}");
    }

    #[test]
    fn a_symlink_planted_in_the_cache_is_refused_by_kind() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().expect("temp dir");
            let store = BlobStore::open(dir.path()).expect("open");
            let (artifact, signature, pubkey) = signed(b"the released installer");
            let digest = store.put(&artifact).expect("put");

            // Move the real blob aside and point its name at it. The contents
            // reached through the link are byte-identical, so digest and
            // signature would both pass — only the file-type check refuses it.
            let path = dir.path().join(format!("{}.tar.gz", digest.to_hex()));
            let elsewhere = dir.path().join("elsewhere.bin");
            std::fs::rename(&path, &elsewhere).expect("move aside");
            std::os::unix::fs::symlink(&elsewhere, &path).expect("symlink");

            let err = store
                .get(
                    &digest.to_hex(),
                    artifact.len() as u64,
                    &signature,
                    &pubkey,
                    HUGE,
                )
                .expect_err("a symlink must not be read as a blob");
            assert!(
                err.to_string().contains("not a regular file"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn a_missing_blob_is_a_cache_miss_rather_than_a_panic() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let absent = FileHash::of_bytes(b"never stored").to_hex();
        assert!(!store.contains(&absent));
        assert!(store.get(&absent, 10, "sig", "key", HUGE).is_err());
    }

    #[test]
    fn a_malformed_digest_never_reaches_the_filesystem() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        for hostile in ["../../etc/passwd", "..", "", "not-hex", "/absolute"] {
            let err = store
                .get(hostile, 10, "sig", "key", HUGE)
                .expect_err("a malformed digest must be refused");
            assert!(
                matches!(err, Error::InvalidHash { .. }),
                "{hostile:?} produced {err}"
            );
        }
    }

    #[test]
    fn retain_keeps_what_it_is_told_to_and_leaves_strangers_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = BlobStore::open(dir.path()).expect("open");
        let (keep, keep_sig, keep_key) = signed(b"the active artifact");
        let (drop, _, _) = signed(b"a superseded artifact");
        let kept = store.put(&keep).expect("put");
        let dropped = store.put(&drop).expect("put");
        std::fs::write(dir.path().join("notes.txt"), b"not ours").expect("write");

        let removed = store
            .retain(&[&kept.to_hex()], std::time::Duration::ZERO)
            .expect("retain");
        assert_eq!(removed, 1);
        assert!(store.contains(&kept.to_hex()));
        assert!(!store.contains(&dropped.to_hex()));
        assert!(
            dir.path().join("notes.txt").is_file(),
            "retain must not tidy away files it does not own"
        );
        // And with a grace period, a freshly written orphan survives -- which
        // is what stops a collection pass deleting a blob another process is
        // between writing and referencing.
        let (fresh, _, _) = signed(b"just written by someone else");
        let fresh_digest = store.put(&fresh).expect("put");
        assert_eq!(
            store
                .retain(&[], std::time::Duration::from_secs(60))
                .expect("retain"),
            0,
            "a blob younger than the grace period must not be collected"
        );
        assert!(store.contains(&fresh_digest.to_hex()));
        store
            .get(
                &kept.to_hex(),
                keep.len() as u64,
                &keep_sig,
                &keep_key,
                HUGE,
            )
            .expect("the retained blob is still intact");
    }

    #[test]
    fn concurrent_writers_of_one_artifact_leave_exactly_one_blob() {
        // The claim that blobs need no lock, run rather than argued.
        use std::sync::Arc;
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Arc::new(BlobStore::open(dir.path()).expect("open"));
        let (artifact, signature, pubkey) = signed(b"the released installer");
        let digest = FileHash::of_bytes(artifact.as_bytes());

        std::thread::scope(|scope| {
            for _ in 0..8 {
                let store = Arc::clone(&store);
                let artifact = artifact.clone();
                scope.spawn(move || {
                    for _ in 0..10 {
                        store.put(&artifact).expect("put");
                    }
                });
            }
        });

        let blobs: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            blobs,
            vec![format!("{}.tar.gz", digest.to_hex())],
            "80 concurrent writes must leave one blob and no temporary files"
        );
        store
            .get(
                &digest.to_hex(),
                artifact.len() as u64,
                &signature,
                &pubkey,
                HUGE,
            )
            .expect("and it must still verify");
    }
}
