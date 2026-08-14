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
//! way the delta path can go *wrong* resolves to [`UpdateSource::Full`], which is
//! exactly what the official updater would have done unaided.
//!
//! [`UpdateSource::Refused`] is the one outcome that is not a fallback, and the
//! distinction is deliberate. A transfer failure says nothing about which
//! release we are being pointed at, so retrying it whole is safe. A downgrade or
//! an identity mismatch says the *pointer itself* is wrong — and the
//! full-download path is pointed at a release by that same document, so falling
//! back would re-run the problem down the other branch. See [`crate::identity`].
//!
//! # One document, one release
//!
//! Everything here is planned from [`UpdateIdentity`], which carries the single
//! response Tauri's own check already fetched. There is no second manifest
//! request, so there is no second answer that could describe a different
//! release. What the manifest is *not* is authenticated — see
//! [`crate::identity`] for exactly what the signature does and does not cover.

use std::path::{Path, PathBuf};

use crate::cache::ArtifactCache;
use crate::identity::{evaluate_version, Refusal, UpdateIdentity, VersionVerdict};
use crate::limits::Limits;
use crate::manifest::{DeltaPlatform, Manifest, TarLayer, TarPatch, TarSupport};
use crate::recompress::recompress_with_limit;
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

/// Everything the planner needs about the host.
///
/// A struct rather than five parameters because the tar path added two more,
/// and a positional list of `Option`s and paths is exactly where a caller
/// eventually passes the wrong one.
pub struct PlanContext<'a> {
    /// Previously cached installer for the **direct** patch path, if the host
    /// keeps one itself. Independent of [`cache`](PlanContext::cache).
    pub base: Option<&'a Path>,

    /// The artifact cache, when the host has one.
    ///
    /// `None` disables the tar path entirely — with no verified base there is
    /// nothing to patch a tar against, which is the ordinary state before a
    /// host's first cached update.
    pub cache: Option<&'a ArtifactCache>,

    /// Base64 minisign public key, as in `tauri.conf.json`.
    ///
    /// Needed here rather than only at install time, because a cached base is
    /// re-verified against it before it is used.
    pub pubkey: &'a str,

    /// Scratch directory. Each update gets its own subdirectory inside it.
    pub work_dir: &'a Path,

    /// Application bundle identifier, as in `tauri.conf.json`'s `identifier`.
    ///
    /// Compared against the identity the signature authenticates, so an artifact
    /// belonging to a different application signed with the same key is refused
    /// rather than installed.
    pub app_id: &'a str,

    /// Local ceilings on what a manifest may ask this host to do.
    pub limits: Limits,
}

/// Which delta paths were actually attempted before falling back.
///
/// Carried on [`UpdateSource::Full`] so a test can tell "the tar path was tried
/// and correctly refused" from "the tar path was never reached". The first is
/// evidence; the second is a fallback test that proves nothing, which is the
/// failure mode that let a delta updater ship without ever deltaing
/// (`docs/DECISIONS.md` #22).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Attempted {
    /// The tar-layer path ran and did not produce an artifact.
    pub tar_delta: bool,
    /// The direct patch path ran and did not produce an artifact.
    pub direct_delta: bool,
}

/// How the update should be obtained.
#[derive(Debug)]
#[must_use = "the caller must act on the chosen source"]
pub enum UpdateSource {
    /// A verified artifact rebuilt through the **tar layer**: a cached base was
    /// expanded, patched, and recompressed back into the published artifact.
    ///
    /// Deliberately a separate variant from [`Delta`](UpdateSource::Delta)
    /// rather than a flag on it. The two paths differ in what they trust (a
    /// local cache, re-verified) and in what they do (recompression, which can
    /// fail on a client whose toolchain does not reproduce the published
    /// bytes), so a caller reporting them as one thing cannot tell which
    /// mechanism was exercised — and neither can a test.
    ///
    /// Has passed the base, tar, size and installer digest gates. The caller
    /// still performs the signature check.
    TarDelta {
        /// The rebuilt artifact.
        artifact: PathBuf,
        /// Owns the per-update workspace the artifact lives in.
        _workspace: tempfile::TempDir,
        /// Signature to validate it against, from the manifest.
        signature: String,
        /// Bytes actually downloaded.
        downloaded: u64,
        /// Bytes a full download would have cost.
        would_have_downloaded: u64,
    },

    /// A verified artifact, already rebuilt from a patch and ready to install.
    ///
    /// It has passed the size and digest gates. The caller still performs the
    /// signature check, which needs the configured public key.
    Delta {
        /// The rebuilt artifact.
        artifact: PathBuf,
        /// Owns the per-update workspace the artifact lives in.
        ///
        /// Held rather than ignored: dropping it removes the directory, so the
        /// artifact stays readable exactly as long as this value is alive and
        /// no longer. Named with a leading underscore because nothing reads it —
        /// its whole contribution is its lifetime.
        _workspace: tempfile::TempDir,
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
        /// Which delta paths ran before this was chosen.
        attempted: Attempted,
    },

    /// The release is already installed. Not an error, and nothing to do.
    ///
    /// There is deliberately no `Unavailable` variant. "Nothing published for
    /// this platform" cannot arise here: an identity only exists because Tauri's
    /// `get_urls` already found a target for it, so a platform with no build
    /// never produces an `Update` to plan from in the first place.
    UpToDate,

    /// The release must not be installed at all.
    ///
    /// Deliberately **not** a fallback. See [`Refusal`] for why each reason
    /// disqualifies the full-download path along with the delta path.
    Refused {
        /// Why the update was refused.
        reason: Refusal,
    },
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
            }
            | Self::TarDelta {
                downloaded,
                would_have_downloaded,
                ..
            } if *would_have_downloaded > 0 => {
                Some(*downloaded as f64 / *would_have_downloaded as f64 * 100.0)
            }
            _ => None,
        }
    }

    /// A short, stable name for which path was chosen.
    ///
    /// For logs and for the E2E harness, which asserts on it. A final hash
    /// alone cannot distinguish a working delta updater from an updater that
    /// silently downloads everything.
    pub fn path_name(&self) -> &'static str {
        match self {
            Self::TarDelta { .. } => "tar-delta",
            Self::Delta { .. } => "delta",
            Self::Full { .. } => "full",
            Self::UpToDate => "up-to-date",
            Self::Refused { .. } => "refused",
        }
    }
}

/// A private scratch directory for one update, removed when the value drops.
///
/// # Why this is a function and not four call sites
///
/// `docs/DECISIONS.md` #18 named three shared filenames — `update.patch`,
/// `update.artifact` and `full.artifact` — and replaced the first two with a
/// per-update `tempdir_in`. **It did not replace the third.** The full-download
/// path kept writing to a fixed name in the shared `work_dir`, so two concurrent
/// updates falling back to a full download raced on one file: blocker B4.
///
/// The rule was stated once and applied twice, which is how the third case
/// survived a decision written specifically about it. It is now applied by
/// calling this, so a future path gets isolation by using the same door rather
/// than by remembering to.
///
/// `tempfile` generates the name from OS randomness and creates the directory
/// with `O_EXCL`, so two callers cannot receive the same path even if they ask at
/// the same instant.
pub fn transaction_workspace(work_dir: &Path) -> Result<tempfile::TempDir, Error> {
    std::fs::create_dir_all(work_dir).map_err(|e| Error::io("create", work_dir, e))?;
    tempfile::Builder::new()
        .prefix("update-")
        .tempdir_in(work_dir)
        .map_err(|e| Error::io("create", work_dir, e))
}

/// Decide how to obtain the release `identity` points at, and if a patch can be
/// used, fetch and apply it.
///
/// `base` is the installer the user already has, if it was cached after the last
/// update. `None` is ordinary — the very first delta update has nothing to patch
/// against, and so does anyone who installed from a download page.
///
/// Never fails. Note that "never fails" is not "always proceeds": a
/// [`UpdateSource::Refused`] is a normal return, and the caller must not treat
/// it as a delta failure to be recovered from.
pub fn plan_update(
    identity: &UpdateIdentity,
    ctx: &PlanContext<'_>,
    fetch: &dyn Fetch,
) -> UpdateSource {
    // The document Tauri fetched, parsed here rather than fetched again. A
    // malformed delta layer disqualifies the delta path only; the full download
    // is described by the identity, which parsed fine by definition.
    let manifest = Manifest::from_json(identity.raw_json()).ok();

    // Bind our reading of that document to Tauri's, and recover the platform
    // entry Tauri actually selected. These are the seams where one document can
    // still be read two ways.
    let mut delta_entry = None;
    if let Some(manifest) = &manifest {
        match resolve_identity(manifest, identity) {
            Ok(entry) => delta_entry = entry,
            Err(reason) => return UpdateSource::Refused { reason },
        }
    }

    // Version policy, applied to the identity's own pair so it is the same
    // comparison Tauri made, on the same document.
    match evaluate_version(identity.current_version(), identity.version()) {
        VersionVerdict::Upgrade => {}
        VersionVerdict::UpToDate => return UpdateSource::UpToDate,
        VersionVerdict::Refused(reason) => return UpdateSource::Refused { reason },
    }

    let mut attempted = Attempted::default();

    // The release identity the signature carries. Read here *unverified*, and
    // used in one direction only: to refuse, or to make the delta paths
    // unavailable. Never to authorise. The authoritative check runs after
    // verification, on VerifiedArtifact::binding.
    let binding = match crate::signature::advisory_binding(identity.signature()) {
        Ok(binding) => binding,
        Err(e) => {
            return UpdateSource::Refused {
                reason: Refusal::ReleaseIdentity {
                    reason: e.to_string(),
                },
            }
        }
    };
    if let Some(signed) = binding.identity() {
        // Everything checkable without the bytes. The digest and size are
        // compared against what the *manifest* declares, which catches a
        // document describing an artifact its own signature contradicts; the
        // comparison against the bytes themselves happens after verification.
        if let Err(e) = signed.check(
            ctx.app_id,
            identity.version(),
            &crate::release_identity::current_platform(),
            &signed.artifact_blake3,
            signed.artifact_size,
        ) {
            return UpdateSource::Refused {
                reason: Refusal::ReleaseIdentity {
                    reason: e.to_string(),
                },
            };
        }
    }

    // The full path always uses the URL and signature Tauri selected, never a
    // platform entry this crate looked up for itself.
    let fallback = |reason: Option<Error>, attempted: Attempted| UpdateSource::Full {
        url: identity.download_url().to_owned(),
        signature: identity.signature().to_owned(),
        reason,
        attempted,
    };

    if manifest.is_none() {
        return fallback(
            Some(Error::Manifest(
                "the delta layer could not be parsed".to_owned(),
            )),
            attempted,
        );
    }

    // No delta layer, or none for the artifact Tauri selected. Ordinary.
    let Some(entry) = delta_entry else {
        return fallback(None, attempted);
    };

    // Refuse an absurd declared size before a byte is downloaded. The manifest
    // is unauthenticated, so its idea of "how big is the target" is a request,
    // not a fact. See crate::limits. Checked once, ahead of both delta paths.
    if let Err(reason) = limits_check(ctx.limits, entry) {
        return fallback(Some(reason), attempted);
    }

    let workspace = |reason: &mut Option<Error>| -> Option<tempfile::TempDir> {
        // One workspace per update, so two concurrent runs cannot consume or
        // overwrite each other's files — see docs/DECISIONS.md #18 and #28.
        match transaction_workspace(ctx.work_dir) {
            Ok(dir) => Some(dir),
            Err(e) => {
                *reason = Some(e);
                None
            }
        }
    };

    // ---- the tar layer, tried first because it is by far the cheapest -----
    //
    // Everything about it is optional: no cache, no layer, a representation
    // this build does not implement, no patch from this version, a cache that
    // does not hold the right base. Each is an ordinary "not this time".
    let mut last_reason = None;
    if !binding.permits_delta() {
        // A legacy signature: valid, and carrying no statement about which
        // release it is. That is a compatibility condition rather than an
        // attack, so the full path stays available exactly as a stock Tauri
        // client would have it -- but the delta paths are ours, and they can
        // require the stronger property. See docs/DECISIONS.md #27.
        return fallback(
            Some(Error::ReleaseIdentity(
                "the signature carries no authenticated release identity, so the \
                 delta paths are unavailable; downloading in full"
                    .to_owned(),
            )),
            attempted,
        );
    }
    if let Some(cache) = ctx.cache {
        match entry.tar_patch(identity.current_version()) {
            Ok(Some((layer, tar_patch))) => {
                // The manifest's representation claim is unauthenticated; the
                // signature's is not. Requiring them to agree is what stops a
                // document describing an artifact as something it is not.
                if let Some(signed) = binding.identity() {
                    if let Err(e) = signed.check_representation(&layer.representation) {
                        return UpdateSource::Refused {
                            reason: Refusal::ReleaseIdentity {
                                reason: e.to_string(),
                            },
                        };
                    }
                }
                attempted.tar_delta = true;
                let mut reason = None;
                if let Some(space) = workspace(&mut reason) {
                    match attempt_tar_delta(
                        entry,
                        layer,
                        tar_patch,
                        cache,
                        ctx.pubkey,
                        space.path(),
                        ctx.limits,
                        fetch,
                    ) {
                        Ok(artifact) => {
                            return UpdateSource::TarDelta {
                                artifact,
                                _workspace: space,
                                signature: identity.signature().to_owned(),
                                downloaded: tar_patch.patch_size,
                                would_have_downloaded: entry.target_installer_size,
                            };
                        }
                        Err(e) => last_reason = Some(e),
                    }
                } else {
                    last_reason = reason;
                }
            }
            Ok(None) => {}
            // A representation or recipe this build does not implement. Not an
            // error, and specifically not a reason to stop: the direct patch
            // below is exactly what such a client is supposed to use.
            Err(unsupported) => last_reason = Some(unsupported_reason(&unsupported)),
        }
    }

    // ---- the direct patch, unchanged in meaning ---------------------------

    let Some(patch) = entry.patches.get(identity.current_version()) else {
        return fallback(last_reason, attempted);
    };
    let Some(base) = ctx.base else {
        return fallback(last_reason, attempted);
    };

    attempted.direct_delta = true;
    let mut reason = None;
    let Some(space) = workspace(&mut reason) else {
        return fallback(reason.or(last_reason), attempted);
    };

    match attempt_delta(entry, patch, base, space.path(), fetch) {
        Ok(artifact) => UpdateSource::Delta {
            artifact,
            _workspace: space,
            // The identity's signature, not the entry's. They were just proven
            // equal, so this is the same value — taking it from the identity
            // keeps the authoritative source obvious at the point of use.
            signature: identity.signature().to_owned(),
            downloaded: patch.patch_size,
            would_have_downloaded: entry.target_installer_size,
        },
        Err(reason) => fallback(Some(reason), attempted),
    }
}

fn limits_check(limits: Limits, entry: &DeltaPlatform) -> Result<(), Error> {
    limits.check_target_size(entry.target_installer_size)
}

fn unsupported_reason(support: &TarSupport) -> Error {
    match support {
        TarSupport::UnknownRepresentation(id) => Error::Manifest(format!(
            "tar layer declares representation {id:?}, which this build does not implement"
        )),
        TarSupport::UnknownRecompression(id) => Error::Manifest(format!(
            "tar layer declares recompression recipe {id:?}, which this build cannot perform"
        )),
        TarSupport::Supported => Error::Manifest("supported".to_owned()),
    }
}

/// Resolve which platform entry Tauri selected, and check it against ours.
///
/// # Why not compute the key
///
/// Tauri searches `{os}-{arch}-{installer}` then `{os}-{arch}`
/// (`updater.rs:578`), builds those strings **locally inside `get_urls`**, and
/// throws them away — it returns only the URL and signature. `Update.target` is
/// not that key: it is `updater_os()`, bare `"darwin"`.
///
/// Gate A used `Update.target` as the key on the assumption it was
/// `{os}-{arch}`. It matched nothing, so every update quietly took the full
/// path. The real-app E2E found it; nothing else had, because falling back is
/// indistinguishable from working unless you look at which path ran.
///
/// So the key is not reconstructed. It is *recovered* from the two values Tauri
/// did keep — the URL and signature it selected — which is the same principle
/// that motivated the whole of `docs/DECISIONS.md` #13: consume Tauri's
/// decision, never re-derive it.
fn resolve_identity<'m>(
    manifest: &'m Manifest,
    identity: &UpdateIdentity,
) -> std::result::Result<Option<&'m crate::manifest::DeltaPlatform>, Refusal> {
    if manifest.version != identity.version() {
        return Err(Refusal::IdentityMismatch {
            field: "version",
            checked: identity.version().to_owned(),
            delta: manifest.version.clone(),
        });
    }

    // Every platform entry describing exactly the artifact Tauri selected.
    // Matching on both fields disambiguates a shared URL with different
    // signatures, and a shared signature under different URLs.
    let keys: Vec<&String> = manifest
        .platforms
        .iter()
        .filter(|(_, p)| p.url == identity.download_url() && p.signature == identity.signature())
        .map(|(key, _)| key)
        .collect();

    if keys.is_empty() {
        // Tauri installed from an entry our parse of the same document does not
        // contain. One document read two ways — the exact failure #13 exists to
        // prevent — so this refuses rather than falling back.
        return Err(Refusal::IdentityMismatch {
            field: "platform",
            checked: identity.download_url().to_owned(),
            delta: "no platform entry matches the artifact Tauri selected".to_owned(),
        });
    }

    let Some(delta) = manifest.delta.as_ref() else {
        return Ok(None); // No delta layer at all: an ordinary full download.
    };

    let entries: Vec<&crate::manifest::DeltaPlatform> = keys
        .iter()
        .filter_map(|key| delta.platforms.get(*key))
        .collect();

    let Some(first) = entries.first().copied() else {
        return Ok(None); // Nothing published for the selected artifact.
    };

    // Duplicate keys pointing at one artifact are fine — a manifest may list
    // both `darwin-aarch64` and `darwin-aarch64-app` for the same bundle. What
    // is not fine is duplicates that disagree, because then "the target" has no
    // single answer and picking one would be a guess.
    let agree = entries.iter().all(|e| {
        e.target_installer_blake3 == first.target_installer_blake3
            && e.target_installer_size == first.target_installer_size
            && e.signature == first.signature
    });
    if !agree {
        return Err(Refusal::AmbiguousPlatform {
            matches: entries.len(),
        });
    }

    // The Gate A cross-check, unchanged: the delta layer must describe the same
    // artifact the Tauri layer does.
    if first.signature != identity.signature() {
        return Err(Refusal::IdentityMismatch {
            field: "signature",
            checked: identity.signature().to_owned(),
            delta: first.signature.clone(),
        });
    }

    Ok(Some(first))
}

/// The tar-layer path, end to end.
///
/// ```text
/// cached ACTIVE .app.tar.gz          re-hashed, re-signature-checked
///   -> exact old tar                 bounded expand, digest checked
///   -> + downloaded tar patch        digest checked before it is applied
///   -> exact new tar                 digest checked
///   -> recompressed .app.tar.gz      digest checked against the manifest
/// ```
///
/// Five gates, and none of them is skippable. The cached artifact is untrusted
/// on reuse; the patch is untrusted input; and the recompression is a recipe
/// this build might not perform correctly on this release's artifacts, which is
/// exactly why its output is checked rather than assumed. Any failure is an
/// ordinary fallback — the caller has a full download to reach for.
#[allow(clippy::too_many_arguments)]
fn attempt_tar_delta(
    entry: &DeltaPlatform,
    layer: &TarLayer,
    patch: &TarPatch,
    cache: &ArtifactCache,
    pubkey: &str,
    work_dir: &Path,
    limits: Limits,
    fetch: &dyn Fetch,
) -> Result<PathBuf, Error> {
    // Both uncompressed sizes are declared by the same unauthenticated document
    // as everything else, and both go on to bound real work: `base_tar_size`
    // sizes the expansion of the cached blob, and `target_tar_size` becomes
    // zstd's window and output bound below. Checked here, first, because a
    // comparison against a local ceiling is the cheapest possible rejection —
    // nothing has been read, expanded or downloaded yet.
    //
    // Gate B checked only the compressed installer, which predates this pipeline
    // existing. See crate::limits.
    limits.check_tar_size(layer.target_tar_size)?;
    limits.check_tar_size(patch.base_tar_size)?;

    // The cached base, re-hashed and re-verified against the key configured
    // now. Being in the cache establishes nothing.
    let Some(base) = cache.active(pubkey)? else {
        return Err(Error::Manifest(
            "no cached base artifact for the tar path".to_owned(),
        ));
    };

    // Is what we have the base this patch was made against? Checked on the
    // compressed artifact and on the tar, both before anything is expanded or
    // downloaded — the cheapest possible rejection of a wrong base.
    if base.entry.compressed_blake3 != patch.base_installer_blake3
        || base.entry.compressed_size != patch.base_installer_size
    {
        return Err(Error::ChecksumMismatch {
            path: PathBuf::from("<cached base installer>"),
            expected: patch.base_installer_blake3.clone(),
            actual: base.entry.compressed_blake3.clone(),
        });
    }
    if base.entry.tar_blake3 != patch.base_tar_blake3 || base.entry.tar_size != patch.base_tar_size
    {
        return Err(Error::ChecksumMismatch {
            path: PathBuf::from("<cached base tar>"),
            expected: patch.base_tar_blake3.clone(),
            actual: base.entry.tar_blake3.clone(),
        });
    }

    std::fs::create_dir_all(work_dir).map_err(|e| Error::io("create", work_dir, e))?;

    // Expanded from the verified bytes, not from the file they came from, and
    // checked against the digest the manifest declares rather than against the
    // one the cache recorded. The cache's own record is a filter; this is the
    // gate.
    let base_tar = work_dir.join("base.tar");
    cache.expand_to_tar(
        &base.artifact,
        &base_tar,
        &patch.base_tar_blake3,
        patch.base_tar_size,
    )?;

    let patch_path = work_dir.join("update.tar.patch");
    fetch
        .fetch(&patch.patch_url, &patch_path)
        .map_err(|e| Error::Fetch(format!("downloading the tar patch: {e}")))?;

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
        layer.target_tar_size,
        &layer.target_tar_blake3,
    )?;
    let target_tar = work_dir.join("target.tar");
    let target_tar = match try_reconstruct(&base_tar, &patch_path, &target_tar, &target) {
        Reconstruction::Verified(path) => path,
        Reconstruction::FallBack(reason) => return Err(reason),
    };
    let _ = std::fs::remove_file(&patch_path);
    let _ = std::fs::remove_file(&base_tar);

    // Back to the published artifact. Built beside the destination and promoted
    // only after the digest gate, so `artifact` never holds unverified bytes —
    // the same discipline `try_reconstruct` applies to the tar.
    let artifact = work_dir.join("update.artifact");
    let building = artifact.with_extension("part");
    // The host's ceiling, not the module's own default. Recompression reads the
    // whole reconstructed tar, so it is the last stage that can be asked to
    // process an unreasonable amount, and the dial a host set for the others
    // should govern it too.
    recompress_with_limit(&target_tar, &building, limits.max_tar_bytes)?;
    let _ = std::fs::remove_file(&target_tar);

    let expected = FileHash::from_hex(&entry.target_installer_blake3)?;
    let rebuilt = FileHash::of_file(&building)?;
    if rebuilt != expected {
        let _ = std::fs::remove_file(&building);
        // The recipe did not reproduce what the release published. On this
        // client, with this toolchain — which is a real possibility and the
        // reason the recipe is versioned and its output checked.
        return Err(Error::ChecksumMismatch {
            path: PathBuf::from("<recompressed installer>"),
            expected: expected.to_hex(),
            actual: rebuilt.to_hex(),
        });
    }
    let size = std::fs::metadata(&building)
        .map_err(|e| Error::io("stat", &building, e))?
        .len();
    if size != entry.target_installer_size {
        let _ = std::fs::remove_file(&building);
        return Err(Error::UnexpectedOutputSize {
            expected: entry.target_installer_size,
            actual: size,
        });
    }

    std::fs::rename(&building, &artifact).map_err(|e| Error::io("promote", &artifact, e))?;
    Ok(artifact)
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

    /// The planner as it was before the cache existed: no cache, no key.
    ///
    /// Every test below this line is about the direct or full path, so a shim
    /// keeps them reading as they did and makes the tar-path tests -- which
    /// pass a real cache -- visibly different.
    fn plan_without_cache(
        identity: &UpdateIdentity,
        base: Option<&Path>,
        work_dir: &Path,
        fetch: &dyn Fetch,
        limits: Limits,
    ) -> UpdateSource {
        plan_update(
            identity,
            &PlanContext {
                base,
                cache: None,
                pubkey: "",
                app_id: APP_ID,
                work_dir,
                limits,
            },
            fetch,
        )
    }
    use std::collections::HashMap;

    use crate::backend::{PatchBackend, ZstdBackend};
    use crate::manifest::{
        DeltaLayer, DeltaPlatform, Patch, TauriPlatform, HASH_ALGO, SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;

    const PLATFORM: &str = "linux-x86_64";
    const APP_ID: &str = "dev.example.testapp";

    /// A real signature over `path`, carrying a real authenticated identity.
    ///
    /// These tests used a placeholder string here. They cannot any more: the
    /// planner reads the release identity out of the signature, so a fixture
    /// that is not a signature is correctly refused. Signing for real also means
    /// every test below now exercises the actual wire format rather than a
    /// stand-in for it.
    fn sign(key: &minisign::KeyPair, path: &Path, version: &str, representation: &str) -> String {
        use base64::Engine as _;
        let identity = crate::release_identity::ReleaseIdentity {
            app_id: APP_ID.to_owned(),
            version: version.to_owned(),
            platform: crate::release_identity::current_platform(),
            representation: representation.to_owned(),
            artifact_blake3: FileHash::of_file(path).expect("hash").to_hex(),
            artifact_size: std::fs::metadata(path).expect("stat").len(),
            signed_at: 1_786_637_312,
        };
        let file = std::fs::File::open(path).expect("open to sign");
        base64::engine::general_purpose::STANDARD.encode(
            minisign::sign(
                None,
                &key.sk,
                file,
                Some(&identity.to_trusted_comment()),
                None,
            )
            .expect("sign")
            .into_string(),
        )
    }
    const FULL_URL: &str = "https://example.com/full.bin";

    /// Serves canned bytes, and can be told to fail.
    struct FakeServer {
        files: HashMap<String, Vec<u8>>,
        /// Every URL requested, in order. Used to prove what was *not* fetched.
        requested: std::cell::RefCell<Vec<String>>,
    }

    impl Fetch for FakeServer {
        fn fetch(&self, url: &str, out: &Path) -> Result<(), String> {
            self.requested.borrow_mut().push(url.to_owned());
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
        /// The real signature over `released`, with its authenticated identity.
        signature: String,
        key: minisign::KeyPair,
    }

    impl Fixture {
        /// The identity a well-behaved server and an honest Tauri check produce
        /// for this release: every field read out of the one document.
        fn identity(&self, current_version: &str) -> UpdateIdentity {
            let tauri = &self.manifest.platforms[PLATFORM];
            UpdateIdentity::new(
                current_version,
                &self.manifest.version,
                PLATFORM,
                &tauri.url,
                &tauri.signature,
                self.manifest.to_json().expect("serialize"),
            )
        }
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

        let key = minisign::KeyPair::generate_encrypted_keypair(Some(String::new()))
            .expect("generate keypair");
        let signature = sign(&key, &new, "1.0.1", "opaque-v1");

        let manifest = Manifest {
            version: "1.0.1".to_owned(),
            notes: None,
            pub_date: None,
            platforms: BTreeMap::from([(
                PLATFORM.to_owned(),
                TauriPlatform {
                    url: FULL_URL.to_owned(),
                    signature: signature.clone(),
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
                        signature: signature.clone(),
                        patches: BTreeMap::from([(
                            "1.0.0".to_owned(),
                            Patch {
                                backend_id: "zstd".to_owned(),
                                patch_url: patch_url.clone(),
                                patch_blake3: FileHash::of_bytes(&patch_bytes).to_hex(),
                                patch_size: patch_bytes.len() as u64,
                            },
                        )]),
                        tar_layer: None,
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
                requested: std::cell::RefCell::new(Vec::new()),
            },
            base: old,
            released: new,
            signature,
            key,
            patch_url,
        }
    }

    /// Generous by default; the size-cap tests set their own.
    fn limits() -> Limits {
        Limits::default()
    }

    fn refusal(source: UpdateSource) -> Refusal {
        match source {
            UpdateSource::Refused { reason } => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // ---- the happy paths, unchanged in substance ------------------------

    #[test]
    fn uses_the_delta_when_everything_lines_up() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );

        let UpdateSource::Delta {
            artifact,
            signature,
            downloaded,
            would_have_downloaded,
            ..
        } = source
        else {
            panic!("expected a delta, got {source:?}");
        };

        assert_eq!(signature, f.signature);
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
        let source = plan_without_cache(
            &f.identity("1.0.0"),
            None,
            &dir.path().join("work"),
            &f.server,
            limits(),
        );

        let UpdateSource::Full { url, reason, .. } = source else {
            panic!("expected a full download, got {source:?}");
        };
        assert_eq!(url, FULL_URL);
        assert!(
            reason.is_none(),
            "a missing base is ordinary, not a failure"
        );
    }

    #[test]
    fn falls_back_when_the_installed_version_has_no_patch() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        // Gate A test 9: no patch published from this version, but the target is
        // a legitimate newer release, so the full path must still be reached.
        let source = plan_without_cache(
            &f.identity("0.1.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        let UpdateSource::Full {
            url,
            signature,
            reason,
            ..
        } = source
        else {
            panic!("expected a full download, got {source:?}");
        };
        assert!(reason.is_none());
        assert_eq!(url, FULL_URL, "the full URL must be Tauri's selection");
        assert_eq!(signature, f.signature);
    }

    #[test]
    fn a_target_several_versions_ahead_still_uses_its_patch() {
        // Gate A test 8. Schema 1 patches straight from any previous version to
        // the latest, so a user far behind is ordinary rather than a special case.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());

        // Re-sign as 9.9.9. The signature authenticates the version now, so a
        // manifest edited to claim a different release without re-signing is
        // correctly refused -- which is the whole point, and makes this fixture
        // have to be a real 9.9.9 release rather than a relabelled 1.0.1.
        f.manifest.version = "9.9.9".to_owned();
        f.signature = sign(&f.key, &f.released, "9.9.9", "opaque-v1");
        f.manifest.platforms.get_mut(PLATFORM).unwrap().signature = f.signature.clone();
        let signature = f.signature.clone();
        let entry = f
            .manifest
            .delta
            .as_mut()
            .unwrap()
            .platforms
            .get_mut(PLATFORM)
            .unwrap();
        entry.target_version = "9.9.9".to_owned();
        entry.signature = signature;
        let patch = entry.patches.remove("1.0.0").expect("fixture patch");
        entry.patches.insert("0.2.0".to_owned(), patch);

        let source = plan_without_cache(
            &f.identity("0.2.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(
            matches!(source, UpdateSource::Delta { .. }),
            "a legitimate multi-version jump should still patch: {source:?}"
        );
    }

    // ---- Gate A: the update identity ------------------------------------

    #[test]
    fn refuses_when_the_checked_version_is_not_the_delta_target() {
        // Gate A test 1, and the core of the double-fetch attack: Tauri was told
        // 9.9.9 so its semver gate would pass, while the document actually
        // describes 1.0.1. Under the old architecture these came from two
        // separate fetches and nothing compared them.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let honest = f.identity("1.0.0");
        let lying = UpdateIdentity::new(
            "1.0.0",
            "9.9.9", // what Tauri was told
            PLATFORM,
            honest.download_url(),
            honest.signature(),
            honest.raw_json(), // what the document says: 1.0.1
        );

        let reason = refusal(plan_without_cache(
            &lying,
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        ));
        assert!(
            matches!(
                &reason,
                Refusal::IdentityMismatch { field: "version", checked, delta }
                    if checked == "9.9.9" && delta == "1.0.1"
            ),
            "expected a version identity mismatch, got {reason:?}"
        );
    }

    #[test]
    fn refuses_when_no_entry_describes_the_artifact_tauri_selected() {
        // Gate A test 2, restated for the corrected resolution model: Tauri
        // reports a signature no platform entry carries, so nothing in the
        // document describes what it selected.
        //
        // The delta-layer-disagrees-with-Tauri-layer case is now unreachable
        // through a parsed manifest, because `Manifest::validate` already
        // rejects a document whose two layers carry different signatures under
        // one key. The cross-check in `resolve_identity` is kept as depth, not
        // because a valid document can trip it.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let honest = f.identity("1.0.0");
        let diverged = UpdateIdentity::new(
            "1.0.0",
            honest.version(),
            PLATFORM,
            honest.download_url(),
            "a-different-signature-tauri-selected",
            honest.raw_json(),
        );

        let reason = refusal(plan_without_cache(
            &diverged,
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        ));
        assert!(
            matches!(
                &reason,
                Refusal::IdentityMismatch {
                    field: "platform",
                    ..
                }
            ),
            "expected a platform identity mismatch, got {reason:?}"
        );
    }

    #[test]
    fn stays_bound_to_the_target_tauri_selected() {
        // Gate A test 3 and the platform-selection finding. Tauri searches
        // `{os}-{arch}-{installer}` before `{os}-{arch}` and does not report
        // which key won. Here both exist with different values, and Tauri picked
        // the installer-specific one. The generic entry must not be substituted
        // just because its key is the one this crate would have computed.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());

        // A second, genuinely signed entry. Minisign includes randomness, so
        // signing the same artifact again yields different signature bytes --
        // which is exactly the shape this test needs, and it has to be a real
        // signature now that the planner reads one.
        let appimage_signature = sign(&f.key, &f.released, "1.0.1", "opaque-v1");
        f.manifest.platforms.insert(
            "linux-x86_64-appimage".to_owned(),
            TauriPlatform {
                url: "https://example.com/real.AppImage".to_owned(),
                signature: appimage_signature.clone(),
            },
        );

        // Tauri selected the AppImage entry; the delta layer describes the
        // generic one.
        let selected = UpdateIdentity::new(
            "1.0.0",
            &f.manifest.version,
            PLATFORM,
            "https://example.com/real.AppImage",
            &appimage_signature,
            f.manifest.to_json().expect("serialize"),
        );

        let source = plan_without_cache(
            &selected,
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );

        // The generic delta entry describes a different artifact, so it must not
        // be used. Publishing no delta for the selected artifact is ordinary, so
        // the correct outcome is a full download from Tauri's own URL — not the
        // refusal the pre-correction model produced.
        let UpdateSource::Full { url, signature, .. } = source else {
            panic!("expected a full download, got {source:?}");
        };
        assert_eq!(url, "https://example.com/real.AppImage");
        assert_eq!(signature, appimage_signature);
    }

    #[test]
    fn the_full_path_always_uses_the_url_tauri_selected() {
        // Even when the delta layer is absent entirely, the fallback URL is the
        // one Tauri resolved — never a platform entry looked up here.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        f.manifest.delta = None;
        // Tauri's selection must correspond to a published entry, so point the
        // document's own entry at the CDN URL.
        f.manifest.platforms.get_mut(PLATFORM).unwrap().url =
            "https://cdn.example.com/selected-by-tauri.AppImage".to_owned();

        let identity = UpdateIdentity::new(
            "1.0.0",
            &f.manifest.version,
            "darwin",
            "https://cdn.example.com/selected-by-tauri.AppImage",
            &f.signature,
            f.manifest.to_json().expect("serialize"),
        );

        let UpdateSource::Full { url, signature, .. } = plan_without_cache(
            &identity,
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        ) else {
            panic!("expected a full download");
        };
        assert_eq!(url, "https://cdn.example.com/selected-by-tauri.AppImage");
        assert_eq!(signature, f.signature);
    }

    // ---- platform resolution, and every way it can be ambiguous ---------
    //
    // The key Tauri used is never reported, so it is recovered from the URL and
    // signature Tauri did keep. These cover each way that recovery can be
    // ambiguous. The rule is: fail closed rather than pick.

    /// Add a second platform key with its own url/signature, and a delta entry.
    fn add_platform(f: &mut Fixture, key: &str, url: &str, sig: &str, blake3: &str, size: u64) {
        f.manifest.platforms.insert(
            key.to_owned(),
            TauriPlatform {
                url: url.to_owned(),
                signature: sig.to_owned(),
            },
        );
        let template = f.manifest.delta.as_ref().unwrap().platforms[PLATFORM].clone();
        f.manifest.delta.as_mut().unwrap().platforms.insert(
            key.to_owned(),
            DeltaPlatform {
                signature: sig.to_owned(),
                target_installer_blake3: blake3.to_owned(),
                target_installer_size: size,
                ..template
            },
        );
    }

    #[test]
    fn a_single_matching_entry_is_selected_whatever_its_key_is_called() {
        // The corrected model in one assertion: the key is "linux-x86_64" while
        // Update.target is "darwin", and resolution still succeeds because it
        // goes by the artifact, not the name.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let identity = UpdateIdentity::new(
            "1.0.0",
            &f.manifest.version,
            "darwin", // bare OS, exactly as Tauri reports it
            FULL_URL,
            &f.signature,
            f.manifest.to_json().expect("serialize"),
        );
        let source = plan_without_cache(
            &identity,
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(
            matches!(source, UpdateSource::Delta { .. }),
            "the artifact Tauri selected is published with a delta: {source:?}"
        );
    }

    #[test]
    fn duplicate_keys_describing_one_artifact_are_accepted() {
        // A manifest may reasonably list both `darwin-aarch64` and
        // `darwin-aarch64-app` for the same bundle. Two matches that agree are
        // not ambiguous — there is one answer, written twice.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        let entry = f.manifest.delta.as_ref().unwrap().platforms[PLATFORM].clone();
        let signature = f.signature.clone();
        add_platform(
            &mut f,
            "linux-x86_64-appimage",
            FULL_URL,
            &signature,
            &entry.target_installer_blake3,
            entry.target_installer_size,
        );

        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(
            matches!(source, UpdateSource::Delta { .. }),
            "identical duplicates have a single answer: {source:?}"
        );
    }

    #[test]
    fn duplicate_keys_disagreeing_about_the_target_are_refused() {
        // Same url and signature, different declared target. There is no
        // principled way to choose, so it must not choose.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        let signature = f.signature.clone();
        add_platform(
            &mut f,
            "linux-x86_64-appimage",
            FULL_URL,
            &signature,
            &FileHash::of_bytes(b"a different target entirely").to_hex(),
            999_999,
        );

        let reason = refusal(plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        ));
        assert!(
            matches!(&reason, Refusal::AmbiguousPlatform { matches: 2 }),
            "expected an ambiguity refusal, got {reason:?}"
        );
    }

    #[test]
    fn the_same_url_under_two_signatures_is_disambiguated_by_signature() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        let entry = f.manifest.delta.as_ref().unwrap().platforms[PLATFORM].clone();
        // Same URL, a different signature — so it describes different bytes.
        add_platform(
            &mut f,
            "linux-x86_64-other",
            FULL_URL,
            "a-different-signature",
            &entry.target_installer_blake3,
            entry.target_installer_size,
        );

        // Tauri selected the original signature, so only one entry matches.
        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(
            matches!(source, UpdateSource::Delta { .. }),
            "got {source:?}"
        );
    }

    #[test]
    fn the_same_signature_under_two_urls_is_disambiguated_by_url() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        let entry = f.manifest.delta.as_ref().unwrap().platforms[PLATFORM].clone();
        let signature = f.signature.clone();
        add_platform(
            &mut f,
            "linux-x86_64-mirror",
            "https://mirror.example.com/full.bin",
            &signature,
            &entry.target_installer_blake3,
            entry.target_installer_size,
        );

        // Tauri selected the original URL, so only one entry matches.
        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(
            matches!(source, UpdateSource::Delta { .. }),
            "got {source:?}"
        );
    }

    // ---- Gate A: version policy -----------------------------------------

    #[test]
    fn refuses_a_downgrade() {
        // Gate A test 4. The manifest describes a real, older, genuinely signed
        // release. Nothing downstream can catch this: that signature is valid
        // and always will be.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let reason = refusal(plan_without_cache(
            &f.identity("2.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        ));
        assert!(
            matches!(&reason, Refusal::Downgrade { current, target }
                if current == "2.0.0" && target == "1.0.1"),
            "expected a downgrade refusal, got {reason:?}"
        );
    }

    #[test]
    fn a_downgrade_never_becomes_a_full_download() {
        // The distinction Gate A turns on: refusing must not degrade into the
        // full path, which is described by the same document.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let source = plan_without_cache(
            &f.identity("2.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(
            !matches!(
                source,
                UpdateSource::Full { .. } | UpdateSource::Delta { .. }
            ),
            "a refusal must not resolve to any install path: {source:?}"
        );
    }

    #[test]
    fn the_same_version_is_up_to_date() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let source = plan_without_cache(
            &f.identity("1.0.1"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(matches!(source, UpdateSource::UpToDate), "got {source:?}");
    }

    #[test]
    fn refuses_an_uncomparable_version() {
        // Gate A test 6. Without an ordering we cannot apply the downgrade
        // policy at all, so proceeding would reopen exactly the hole it closes.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let reason = refusal(plan_without_cache(
            &f.identity("not-a-version"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        ));
        assert!(
            matches!(
                &reason,
                Refusal::UncomparableVersion {
                    which: "installed",
                    ..
                }
            ),
            "expected an uncomparable-version refusal, got {reason:?}"
        );
    }

    #[test]
    fn refuses_an_uncomparable_target_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        f.manifest.version = "1.0".to_owned();
        f.manifest
            .delta
            .as_mut()
            .unwrap()
            .platforms
            .get_mut(PLATFORM)
            .unwrap()
            .target_version = "1.0".to_owned();

        let reason = refusal(plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        ));
        assert!(
            matches!(
                &reason,
                Refusal::UncomparableVersion {
                    which: "target",
                    ..
                }
            ),
            "expected an uncomparable target, got {reason:?}"
        );
    }

    // ---- Gate A: no second fetch ----------------------------------------

    #[test]
    fn never_fetches_the_manifest() {
        // Gate A test 10. The only request a delta plan may make is the patch.
        // There is no manifest URL parameter any more, so this asserts the
        // shape of the flow rather than a policy that could be forgotten.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let _ = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );

        let requested = f.server.requested.borrow();
        assert_eq!(
            requested.len(),
            1,
            "exactly one request — the patch — expected, got {requested:?}"
        );
        assert_eq!(requested[0], f.patch_url);
    }

    #[test]
    fn a_refusal_makes_no_network_request_at_all() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let _ = plan_without_cache(
            &f.identity("2.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(
            f.server.requested.borrow().is_empty(),
            "a refused update must not download anything"
        );
    }

    // ---- delta failures still fall back ---------------------------------

    #[test]
    fn falls_back_when_the_patch_download_fails() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        f.server.files.clear(); // every request 404s

        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
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

        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
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

        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&wrong),
            &dir.path().join("work"),
            &f.server,
            limits(),
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
    fn falls_back_when_the_delta_layer_is_unparseable() {
        // A broken delta layer costs the delta path, not the update: the full
        // download is described by the identity, which parsed by definition.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let identity = UpdateIdentity::new(
            "1.0.0",
            "1.0.1",
            PLATFORM,
            FULL_URL,
            &f.signature,
            "{ not json at all",
        );

        let UpdateSource::Full { url, reason, .. } = plan_without_cache(
            &identity,
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        ) else {
            panic!("expected a full download");
        };
        assert_eq!(url, FULL_URL);
        assert!(matches!(reason, Some(Error::Manifest(_))));
    }

    #[test]
    fn a_selected_artifact_with_no_delta_entry_reaches_the_full_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());

        // The Tauri layer publishes this artifact; the delta layer does not.
        // That is an ordinary full download, not a refusal.
        f.manifest.delta.as_mut().unwrap().platforms.clear();
        let identity = UpdateIdentity::new(
            "1.0.0",
            &f.manifest.version,
            "darwin",
            FULL_URL,
            &f.signature,
            f.manifest.to_json().expect("serialize"),
        );
        let source = plan_without_cache(
            &identity,
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );
        assert!(
            matches!(source, UpdateSource::Full { .. }),
            "got {source:?}"
        );
    }

    #[test]
    fn the_workspace_is_gone_once_the_update_is_done() {
        // Stronger than the old "the patch file is not there" check, which the
        // per-update workspace made vacuous by moving the patch into a
        // subdirectory the assertion never looked in.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());
        let work = dir.path().join("work");

        {
            let source = plan_without_cache(
                &f.identity("1.0.0"),
                Some(&f.base),
                &work,
                &f.server,
                limits(),
            );
            assert!(matches!(source, UpdateSource::Delta { .. }));
            let leftovers: Vec<_> = std::fs::read_dir(&work)
                .expect("work dir")
                .filter_map(|e| e.ok())
                .collect();
            assert_eq!(leftovers.len(), 1, "one workspace while the update is live");
        }

        // The source has been dropped, so the workspace goes with it.
        let leftovers: Vec<_> = std::fs::read_dir(&work)
            .expect("work dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the workspace must not outlive the update: {leftovers:?}"
        );
    }

    #[test]
    fn an_absurd_declared_target_size_is_refused_before_anything_is_downloaded() {
        // The §6 attack. The manifest asks us to reconstruct half a terabyte;
        // no digest or signature check helps, because the resources are spent
        // long before either runs. Only a ceiling the server does not control
        // stops it.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = fixture(dir.path());
        f.manifest
            .delta
            .as_mut()
            .unwrap()
            .platforms
            .get_mut(PLATFORM)
            .unwrap()
            .target_installer_size = 500 * 1024 * 1024 * 1024;

        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );

        let UpdateSource::Full { reason, .. } = source else {
            panic!("expected the delta to be refused, got {source:?}");
        };
        assert!(
            matches!(reason, Some(Error::DeclaredSizeTooLarge { .. })),
            "expected the local ceiling to refuse it, got {reason:?}"
        );
        assert!(
            f.server.requested.borrow().is_empty(),
            "the patch must not be downloaded before the size is judged"
        );
    }

    #[test]
    fn the_local_ceiling_is_not_the_manifests_to_raise() {
        // The invariant in one assertion: a *declared* size within the manifest
        // but beyond the host's ceiling loses. Server-controlled limits are the
        // thing this defends against, so the host's number has to win.
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());
        let declared = f.manifest.delta.as_ref().unwrap().platforms[PLATFORM].target_installer_size;

        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            Limits {
                max_target_bytes: declared - 1,
                ..Limits::default()
            },
        );
        assert!(matches!(
            source,
            UpdateSource::Full {
                reason: Some(Error::DeclaredSizeTooLarge { .. }),
                ..
            }
        ));
    }

    #[test]
    fn two_concurrent_updates_do_not_consume_each_others_files() {
        // Both runs share a work_dir, as two windows of one app or two instances
        // would. With fixed filenames the second run overwrites the first's
        // artifact between its digest check and its read, and the first fails
        // its signature check — a benign double-click turning into the one
        // failure the design refuses to recover from (DECISIONS #11).
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());
        let work = dir.path().join("shared-work");

        let first = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &work,
            &f.server,
            limits(),
        );
        let second = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &work,
            &f.server,
            limits(),
        );

        let (UpdateSource::Delta { artifact: a, .. }, UpdateSource::Delta { artifact: b, .. }) =
            (&first, &second)
        else {
            panic!("both runs should reach a delta: {first:?} / {second:?}");
        };

        assert_ne!(a, b, "each update must get its own artifact path");
        let expected = FileHash::of_file(&f.released).expect("hash released");
        assert_eq!(FileHash::of_file(a).expect("hash a"), expected);
        assert_eq!(
            FileHash::of_file(b).expect("hash b"),
            expected,
            "the second run must not have disturbed the first"
        );
    }

    #[test]
    fn reports_how_much_was_saved() {
        let dir = tempfile::tempdir().expect("temp dir");
        let f = fixture(dir.path());

        let source = plan_without_cache(
            &f.identity("1.0.0"),
            Some(&f.base),
            &dir.path().join("work"),
            &f.server,
            limits(),
        );

        let percent = source
            .downloaded_percent()
            .expect("a delta should report its saving");
        assert!(
            percent > 0.0 && percent < 100.0,
            "implausible saving: {percent}%"
        );
        assert!(
            UpdateSource::UpToDate.downloaded_percent().is_none(),
            "only a delta has a saving to report"
        );
    }
}
