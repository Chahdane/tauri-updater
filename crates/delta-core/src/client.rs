//! The client half of an update: decide, fetch, rebuild, verify.
//!
//! This is what the Tauri plugin calls. It is deliberately free of any Tauri
//! dependency and of any particular HTTP stack — transport arrives through the
//! [`Fetch`] trait — for two reasons. It keeps the flow testable on any machine
//! without a running app or a network, and it keeps the part that decides
//! *what to install* separate from the part that knows *how to install it*,
//! which is the only genuinely per-platform piece.
//!
//! [`plan_update`] cannot fail. It returns an [`UpdateSource`], and the caller's
//! job is to act on whichever variant it gets — never to handle an error. Every
//! way the delta path can go wrong resolves to
//! [`UpdateSource::Full`], which is exactly what the official updater would have
//! done unaided.

use std::path::{Path, PathBuf};

use crate::manifest::Manifest;
use crate::{try_reconstruct, Error, FileHash, Reconstruction, TargetSpec};

/// Fetches a URL into a local file.
///
/// Implemented by the host — the plugin uses Tauri's HTTP stack, tests use a
/// map of canned responses. Errors are reported as plain strings because the
/// only thing this layer does with a transport failure is fall back.
pub trait Fetch {
    /// Download `url`, writing the body to `out`.
    fn fetch(&self, url: &str, out: &Path) -> Result<(), String>;
}

/// How the update should be obtained.
#[derive(Debug)]
#[must_use = "the caller must act on the chosen source"]
pub enum UpdateSource {
    /// A verified artifact, already rebuilt from a patch and ready to install.
    ///
    /// It has passed the size and digest gates. The caller still performs the
    /// signature check, which needs the configured public key.
    Delta {
        /// The rebuilt artifact.
        artifact: PathBuf,
        /// Signature to validate it against, from the manifest.
        signature: String,
        /// Bytes actually downloaded.
        downloaded: u64,
        /// Bytes a full download would have cost.
        would_have_downloaded: u64,
    },

    /// Download the complete artifact, as normal.
    Full {
        /// Where to download it from.
        url: String,
        /// Signature to validate it against.
        signature: String,
        /// Why the delta path was not taken, when it was attempted at all.
        ///
        /// For logging only. Every reason leads to the same action.
        reason: Option<Error>,
    },

    /// The manifest publishes nothing for this platform.
    Unavailable,
}

impl UpdateSource {
    /// Fraction of a full download that was actually transferred, if a delta
    /// was used.
    pub fn downloaded_percent(&self) -> Option<f64> {
        match self {
            Self::Delta {
                downloaded,
                would_have_downloaded,
                ..
            } if *would_have_downloaded > 0 => {
                Some(*downloaded as f64 / *would_have_downloaded as f64 * 100.0)
            }
            _ => None,
        }
    }
}

/// Decide how to obtain the release described by `manifest`, and if a patch can
/// be used, fetch and apply it.
///
/// `base` is the installer the user already has, if it was cached after the last
/// update. `None` is ordinary — the very first delta update has nothing to patch
/// against, and so does anyone who installed from a download page.
///
/// Never fails.
pub fn plan_update(
    manifest: &Manifest,
    platform: &str,
    installed_version: &str,
    base: Option<&Path>,
    work_dir: &Path,
    fetch: &dyn Fetch,
) -> UpdateSource {
    // No build for this platform at all — nothing to do, delta or otherwise.
    let Some(full) = manifest.platforms.get(platform) else {
        return UpdateSource::Unavailable;
    };

    let fallback = |reason: Option<Error>| UpdateSource::Full {
        url: full.url.clone(),
        signature: full.signature.clone(),
        reason,
    };

    // Each of these is an ordinary "no patch for you", not a failure: the
    // manifest may publish no delta layer, none for this platform, or none from
    // the version this user is on.
    let Some((entry, patch)) = manifest.patch_for(platform, installed_version) else {
        return fallback(None);
    };

    let Some(base) = base else {
        return fallback(None);
    };

    match attempt_delta(entry, patch, base, work_dir, fetch) {
        Ok(artifact) => UpdateSource::Delta {
            artifact,
            signature: entry.signature.clone(),
            downloaded: patch.patch_size,
            would_have_downloaded: entry.target_installer_size,
        },
        Err(reason) => fallback(Some(reason)),
    }
}

fn attempt_delta(
    entry: &crate::manifest::DeltaPlatform,
    patch: &crate::manifest::Patch,
    base: &Path,
    work_dir: &Path,
    fetch: &dyn Fetch,
) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(work_dir).map_err(|e| Error::io("create", work_dir, e))?;

    let patch_path = work_dir.join("update.patch");
    fetch
        .fetch(&patch.patch_url, &patch_path)
        .map_err(|e| Error::Fetch(format!("downloading the patch: {e}")))?;

    // Check the patch against the manifest before spending anything applying
    // it. A truncated or tampered download is caught here, cheaply.
    let expected = FileHash::from_hex(&patch.patch_blake3)?;
    let actual = FileHash::of_file(&patch_path)?;
    if actual != expected {
        return Err(Error::ChecksumMismatch {
            path: patch_path,
            expected: expected.to_hex(),
            actual: actual.to_hex(),
        });
    }

    let target = TargetSpec::new(
        &patch.backend_id,
        entry.target_installer_size,
        &entry.target_installer_blake3,
    )?;

    let artifact = work_dir.join("update.artifact");
    match try_reconstruct(base, &patch_path, &artifact, &target) {
        Reconstruction::Verified(path) => {
            // The patch has done its job; it can be large.
            let _ = std::fs::remove_file(&patch_path);
            Ok(path)
        }
        Reconstruction::FallBack(reason) => {
            let _ = std::fs::remove_file(&patch_path);
            Err(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::backend::{PatchBackend, ZstdBackend};
    use crate::manifest::{
        DeltaLayer, DeltaPlatform, Patch, TauriPlatform, HASH_ALGO, SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;

    const PLATFORM: &str = "linux-x86_64";
    const SIGNATURE: &str = "a-signature";

    /// Serves canned bytes, and can be told to fail.
    struct FakeServer {
        files: HashMap<String, Vec<u8>>,
    }

    impl Fetch for FakeServer {
        fn fetch(&self, url: &str, out: &Path) -> Result<(), String> {
            let body = self.files.get(url).ok_or_else(|| format!("404 {url}"))?;
            std::fs::write(out, body).map_err(|e| e.to_string())
        }
    }

    /// A real release: two artifacts, a real patch, and a manifest describing it.
    struct Fixture {
        manifest: Manifest,
        server: FakeServer,
        base: PathBuf,
        released: PathBuf,
        patch_url: String,
    }

    fn fixture(dir: &Path) -> Fixture {
        let old = dir.join("old.bin");
        let new = dir.join("new.bin");
        // Small but genuinely different, with a shared region so the patch is
        // a real delta rather than a re-compression.
        let shared: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        let mut old_bytes = shared.clone();
        old_bytes.extend_from_slice(b"version one tail");
        let mut new_bytes = shared;
        new_bytes.extend_from_slice(b"version two tail, slightly longer");
        std::fs::write(&old, &old_bytes).expect("write old");
        std::fs::write(&new, &new_bytes).expect("write new");

        let patch = dir.join("real.patch");
        ZstdBackend::new()
            .with_level(3)
            .diff(&old, &new, &patch)
            .expect("diff");

        let patch_bytes = std::fs::read(&patch).expect("read patch");
        let patch_url = "https://example.com/update.patch".to_owned();

        let manifest = Manifest {
            version: "1.0.1".to_owned(),
            notes: None,
            pub_date: None,
            platforms: BTreeMap::from([(
                PLATFORM.to_owned(),
                TauriPlatform {
                    url: "https://example.com/full.bin".to_owned(),
                    signature: SIGNATURE.to_owned(),
                },
            )]),
            delta: Some(DeltaLayer {
                schema: SCHEMA_VERSION,
                hash_algo: HASH_ALGO.to_owned(),
                platforms: BTreeMap::from([(
                    PLATFORM.to_owned(),
                    DeltaPlatform {
                        target_version: "1.0.1".to_owned(),
                        target_installer_blake3: FileHash::of_file(&new)
                            .expect("hash new")
                            .to_hex(),
                        target_installer_size: new_bytes.len() as u64,
                        signature: SIGNATURE.to_owned(),
                        patches: BTreeMap::from([(
                            "1.0.0".to_owned(),
                            Patch {
                                backend_id: "zstd".to_owned(),
                                patch_url: patch_url.clone(),
                                patch_blake3: FileHash::of_bytes(&patch_bytes).to_hex(),
                                patch_size: patch_bytes.len() as u64,
                            },
                        )]),
                    },
                )]),
            }),
        };
        manifest
            .validate()
            .expect("fixture manifest should be valid");

        Fixture {
            manifest,
            server: FakeServer {
                files: HashMap::from([(patch_url.clone(), patch_bytes)]),
            },
            base: old,
            released: new,
            patch_url,
        }
    }

    #[test]
    fn uses_the_delta_when_everything_lines_up() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let source = plan_update(
            &f.manifest,
            PLATFORM,
            "1.0.0",
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
        );

        let UpdateSource::Delta {
            artifact,
            signature,
            downloaded,
            would_have_downloaded,
        } = source
        else {
            panic!("expected a delta, got {source:?}");
        };

        assert_eq!(signature, SIGNATURE);
        assert!(downloaded < would_have_downloaded);

        // And it really is the released artifact.
        assert_eq!(
            FileHash::of_file(&artifact).expect("hash rebuilt"),
            FileHash::of_file(&f.released).expect("hash released"),
        );
    }

    #[test]
    fn falls_back_when_there_is_no_cached_base() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        // The common case for a user's first delta update. Not an error.
        let source = plan_update(
            &f.manifest,
            PLATFORM,
            "1.0.0",
            None,
            &dir.path().join("work"),
            &f.server,
        );

        let UpdateSource::Full { url, reason, .. } = source else {
            panic!("expected a full download, got {source:?}");
        };
        assert_eq!(url, "https://example.com/full.bin");
        assert!(
            reason.is_none(),
            "a missing base is ordinary, not a failure"
        );
    }

    #[test]
    fn falls_back_when_the_installed_version_has_no_patch() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let source = plan_update(
            &f.manifest,
            PLATFORM,
            "0.1.0",
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
        );
        assert!(matches!(source, UpdateSource::Full { reason: None, .. }));
    }

    #[test]
    fn falls_back_when_the_patch_download_fails() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        f.server.files.clear(); // every request 404s

        let source = plan_update(
            &f.manifest,
            PLATFORM,
            "1.0.0",
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
        );

        let UpdateSource::Full { reason, .. } = source else {
            panic!("expected a full download, got {source:?}");
        };
        assert!(matches!(reason, Some(Error::Fetch(_))));
    }

    #[test]
    fn falls_back_when_the_patch_does_not_match_its_digest() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        // A tampered or truncated download that still returns 200.
        f.server
            .files
            .insert(f.patch_url.clone(), b"not the patch at all".to_vec());

        let source = plan_update(
            &f.manifest,
            PLATFORM,
            "1.0.0",
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
        );

        let UpdateSource::Full { reason, .. } = source else {
            panic!("expected a full download, got {source:?}");
        };
        assert!(
            matches!(reason, Some(Error::ChecksumMismatch { .. })),
            "the patch digest must be checked before it is applied: {reason:?}"
        );
    }

    #[test]
    fn falls_back_when_the_base_is_the_wrong_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());
        let wrong = dir.path().join("wrong.bin");
        std::fs::write(&wrong, b"a completely different installer").expect("write");

        let source = plan_update(
            &f.manifest,
            PLATFORM,
            "1.0.0",
            Some(&wrong),
            &dir.path().join("work"),
            &f.server,
        );
        assert!(matches!(
            source,
            UpdateSource::Full {
                reason: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn reports_unavailable_for_a_platform_with_no_build() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let source = plan_update(
            &f.manifest,
            "windows-x86_64",
            "1.0.0",
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
        );
        assert!(matches!(source, UpdateSource::Unavailable));
    }

    #[test]
    fn does_not_leave_the_patch_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());
        let work = dir.path().join("work");

        let _ = plan_update(
            &f.manifest,
            PLATFORM,
            "1.0.0",
            Some(&f.base),
            &work,
            &f.server,
        );

        assert!(
            !work.join("update.patch").exists(),
            "a patch is dead weight once applied and should not be kept"
        );
    }

    #[test]
    fn reports_how_much_was_saved() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let source = plan_update(
            &f.manifest,
            PLATFORM,
            "1.0.0",
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
        );

        let percent = source
            .downloaded_percent()
            .expect("a delta should report its saving");
        assert!(
            percent > 0.0 && percent < 100.0,
            "implausible saving: {percent}%"
        );
        assert!(
            UpdateSource::Unavailable.downloaded_percent().is_none(),
            "only a delta has a saving to report"
        );
    }
}
