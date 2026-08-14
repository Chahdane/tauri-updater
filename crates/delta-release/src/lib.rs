//! Release-time tooling: turn two installers into a patch and a manifest.
//!
//! This is the half of the system that runs on CI, once per release. It takes
//! the installer users already have and the installer they are moving to, and
//! produces everything a client needs to make that move cheaply:
//!
//! - the patch itself,
//! - the digests that prove a reconstruction is correct,
//! - a minisign signature over the **target installer**, and
//! - a manifest that is simultaneously a valid Tauri updater document.
//!
//! Nothing here runs on a user's machine, so it favours being obvious over being
//! fast.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod signing;

pub mod tar_layer;

use std::path::Path;

use tauri_updater_delta_core::backend::{PatchBackend, ZstdBackend};
use tauri_updater_delta_core::manifest::{
    DeltaLayer, DeltaPlatform, Manifest, Patch, TauriPlatform, HASH_ALGO, SCHEMA_VERSION,
};
use tauri_updater_delta_core::FileHash;

use signing::SigningKey;

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can go wrong while preparing a release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed.
    #[error("{0}")]
    Io(String),

    /// A signing key could not be loaded.
    #[error("{0}")]
    Key(String),

    /// Producing a signature failed.
    #[error("{0}")]
    Sign(String),

    /// The requested release does not make sense.
    #[error("{0}")]
    Request(String),

    /// The delta engine failed.
    #[error(transparent)]
    Engine(#[from] tauri_updater_delta_core::Error),
}

/// A release to publish, with or without an upgrade path into it.
#[derive(Debug, Clone)]
pub struct ReleaseRequest<'a> {
    /// Tauri platform identifier, e.g. `"linux-x86_64"`.
    pub platform: &'a str,
    /// Version being released.
    pub version: &'a str,
    /// Installer being released.
    pub new_installer: &'a Path,
    /// Where the full installer will be downloadable.
    pub installer_url: &'a str,
    /// Optional release notes.
    pub notes: Option<&'a str>,
    /// Optional RFC 3339 publication timestamp.
    pub pub_date: Option<&'a str>,

    /// Application bundle identifier, bound into the signature's authenticated
    /// release identity.
    ///
    /// Required, and deliberately not inferred from the artifact. macOS could be
    /// read out of `Contents/Info.plist`, but that is one bundle format on one
    /// platform, and a security field derived by guessing is a security field
    /// that is sometimes wrong. Callers pass their `tauri.conf.json`
    /// `identifier`; the release workflow reads it from the same file.
    pub app_id: &'a str,

    /// The release users are upgrading **from**, when there is one.
    ///
    /// # Why this is optional, and why that is the whole of blocker B5
    ///
    /// These four fields used to be required, so a release with no predecessor
    /// could not be *expressed* — not merely "produced no patch". The workflow
    /// dealt with that by skipping the whole release step when no previous tag
    /// existed, which skipped the manifest with it. A first release therefore
    /// published an artifact with **no updater document and no signature**, and
    /// every client that checked for updates found nothing to check.
    ///
    /// The delta layer is the optional part of a release. The updater document
    /// is not: it is the thing Tauri reads. Making the predecessor an `Option`
    /// puts that distinction in the type, so "no previous release" produces a
    /// complete, signed, Full-only manifest and cannot silently produce nothing.
    pub predecessor: Option<Predecessor<'a>>,
}

/// The previous release, and where to put the patches generated against it.
#[derive(Debug, Clone)]
pub struct Predecessor<'a> {
    /// Version this patch upgrades from.
    pub from_version: &'a str,
    /// Installer users on `from_version` already have.
    pub installer: &'a Path,
    /// Where the patch will be downloadable.
    pub patch_url: &'a str,
    /// Where to write the generated patch.
    pub patch_out: &'a Path,

    /// Also publish a tar-layer patch, when the artifacts support one.
    ///
    /// Lives here rather than beside `platform` because a tar patch is a patch:
    /// it needs two artifacts, so it cannot exist without a predecessor. The
    /// type says so.
    pub tar_layer: Option<TarLayerOptions<'a>>,
}

/// Where a tar-layer patch should be written and served from.
#[derive(Debug, Clone)]
pub struct TarLayerOptions<'a> {
    /// Where the tar patch will be downloadable.
    pub patch_url: &'a str,
    /// Where to write the generated tar patch.
    pub patch_out: &'a Path,
    /// Scratch directory. Defaults to `.delta-tar-work` beside `patch_out`.
    pub work_dir: Option<&'a Path>,
    /// Largest tar this run will expand.
    pub max_tar_bytes: u64,
    /// Fail the release if a tar layer cannot be produced.
    ///
    /// Off by default, because "these artifacts are not tarballs" is an
    /// ordinary answer for most platforms. On, it turns a silent absence into
    /// a build failure — which is what a project that has decided to depend on
    /// the tar layer wants, since a missing layer is invisible in the manifest.
    pub required: bool,
}

/// What a release run produced, beyond the manifest itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchSummary {
    /// Size of the generated patch, when there was a predecessor to patch from.
    pub patch_size: Option<u64>,
    /// Size of the target installer in bytes.
    pub installer_size: u64,
    /// Size of the tar-layer patch, if one was published.
    pub tar_patch_size: Option<u64>,
    /// Why no tar layer was published, when one was asked for.
    ///
    /// Carried rather than logged so a caller can decide whether it matters.
    /// Every reason is ordinary unless [`TarLayerOptions::required`] is set.
    pub tar_layer_skipped: Option<String>,
}

impl PatchSummary {
    /// Patch size as a percentage of a full download.
    ///
    /// `None` when this release published no patch, which is a different thing
    /// from a patch of zero bytes.
    pub fn ratio_percent(&self) -> Option<f64> {
        match self.patch_size {
            Some(_) if self.installer_size == 0 => Some(0.0),
            Some(patch) => Some(patch as f64 / self.installer_size as f64 * 100.0),
            None => None,
        }
    }
}

/// Generate a patch and fold it into a manifest.
///
/// `existing` is the manifest currently published, if any. Because schema 1
/// only supports patching *to the latest release*, a manifest describing an
/// older version is replaced rather than merged — its patches reconstruct an
/// artifact that is no longer current. A manifest for the same version has the
/// new upgrade path added alongside the ones already there, which is what lets a
/// release publish several `from` versions by calling this repeatedly.
pub fn build_release(
    req: &ReleaseRequest<'_>,
    key: &SigningKey,
    existing: Option<Manifest>,
) -> Result<(Manifest, PatchSummary)> {
    if !req.new_installer.is_file() {
        return Err(Error::Request(format!(
            "new installer {} does not exist",
            req.new_installer.display()
        )));
    }
    if let Some(pred) = &req.predecessor {
        if pred.from_version == req.version {
            return Err(Error::Request(format!(
                "cannot patch {} to itself",
                req.version
            )));
        }
        if !pred.installer.is_file() {
            return Err(Error::Request(format!(
                "previous installer {} does not exist",
                pred.installer.display()
            )));
        }
    }

    let installer_digest = FileHash::of_file(req.new_installer)?;
    let installer_size = file_size(req.new_installer)?;

    // Over the target installer, not the patch: both the delta path and the
    // full-download path end up holding this exact artifact, so one signature
    // covers both.
    //
    // The signature now also carries the release identity, so it says which
    // release these bytes are rather than only that they are ours. See
    // `docs/DECISIONS.md` #27.
    let representation = if tar_layer::looks_like_app_tar_gz(req.new_installer) {
        tauri_updater_delta_core::manifest::REPRESENTATION_APP_TAR_GZ_V1
    } else {
        tauri_updater_delta_core::release_identity::REPRESENTATION_OPAQUE_V1
    };
    let signature = key.sign_release(
        req.new_installer,
        &signing::ReleaseFacts {
            app_id: req.app_id,
            version: req.version,
            platform: req.platform,
            representation,
        },
    )?;

    // The direct patch, generated and then *proven* before it is described.
    // Blocker B7: this used to emit metadata for a patch nobody had ever
    // applied. See `generate_direct_patch`.
    let direct = match &req.predecessor {
        Some(pred) => Some(generate_direct_patch(
            pred,
            req.new_installer,
            installer_digest,
        )?),
        None => None,
    };

    let mut manifest = match existing {
        Some(existing) if existing.version == req.version => existing,
        _ => Manifest {
            version: req.version.to_owned(),
            notes: None,
            pub_date: None,
            platforms: Default::default(),
            delta: None,
        },
    };

    if let Some(notes) = req.notes {
        manifest.notes = Some(notes.to_owned());
    }
    if let Some(pub_date) = req.pub_date {
        manifest.pub_date = Some(pub_date.to_owned());
    }

    manifest.platforms.insert(
        req.platform.to_owned(),
        TauriPlatform {
            url: req.installer_url.to_owned(),
            signature: signature.clone(),
        },
    );

    let delta = manifest.delta.get_or_insert_with(|| DeltaLayer {
        schema: SCHEMA_VERSION,
        hash_algo: HASH_ALGO.to_owned(),
        platforms: Default::default(),
    });

    let entry = delta
        .platforms
        .entry(req.platform.to_owned())
        .or_insert_with(|| DeltaPlatform {
            target_version: req.version.to_owned(),
            target_installer_blake3: installer_digest.to_hex(),
            target_installer_size: installer_size,
            signature: signature.clone(),
            patches: Default::default(),
            tar_layer: None,
        });

    // Re-signing produces a different signature each run (minisign includes
    // randomness), so keep the entry and the Tauri layer in step explicitly
    // rather than relying on them having been written together.
    entry.target_version = req.version.to_owned();
    entry.target_installer_blake3 = installer_digest.to_hex();
    entry.target_installer_size = installer_size;
    entry.signature = signature;

    let mut patch_size = None;
    if let (Some(pred), Some(patch)) = (&req.predecessor, direct) {
        patch_size = Some(patch.patch_size);
        entry.patches.insert(pred.from_version.to_owned(), patch);
    }

    // The tar layer is strictly additive: if anything about it fails, the entry
    // above is already complete and the release publishes without it.
    let mut tar_patch_size = None;
    let mut tar_layer_skipped = None;
    if let Some((pred, options)) = req
        .predecessor
        .as_ref()
        .and_then(|p| p.tar_layer.as_ref().map(|t| (p, t)))
    {
        match build_tar_layer(req, pred, options, entry.tar_layer.take()) {
            Ok((layer, size)) => {
                tar_patch_size = Some(size);
                entry.tar_layer = Some(layer);
            }
            Err(reason) => {
                let reason = reason.to_string();
                if options.required {
                    return Err(Error::Request(format!(
                        "a tar layer was required and could not be produced: {reason}"
                    )));
                }
                // Leave the entry with no tar layer at all rather than a stale
                // one: a layer describing the previous release would tell every
                // client to reconstruct the wrong tar.
                tar_layer_skipped = Some(reason);
            }
        }
    }

    manifest.validate()?;

    Ok((
        manifest,
        PatchSummary {
            patch_size,
            installer_size,
            tar_patch_size,
            tar_layer_skipped,
        },
    ))
}

/// Generate the direct patch, then prove it reconstructs the target exactly.
///
/// # Blocker B7
///
/// This function used to be four lines: diff, hash the patch, record the size,
/// emit the metadata. Every number in that metadata was correct *about the patch
/// file* and none of it was evidence that applying the patch produced anything
/// in particular. The tar layer had round-tripped its own output since it was
/// written — `tar_layer.rs` said so, and said the direct generator did not — so
/// the release tool shipped one path that proved itself and one that asserted
/// itself.
///
/// What the round-trip catches that the digests cannot: a backend whose `diff`
/// and `apply` disagree, a compression setting the client will not accept, a
/// truncated write that still hashes consistently, and any future change to the
/// engine that breaks reconstruction without breaking generation. All of those
/// produce a perfectly well-formed manifest describing a patch that fails on
/// every client — and because a failed delta falls back to a full download, the
/// symptom is not an error anyone sees. It is every user silently paying full
/// price, which is exactly the failure `docs/DECISIONS.md` #22 is about.
///
/// The cost is one patch application per upgrade path at release time, on CI.
fn generate_direct_patch(
    pred: &Predecessor<'_>,
    new_installer: &Path,
    expected: FileHash,
) -> Result<Patch> {
    if let Some(parent) = pred.patch_out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("creating {}: {e}", parent.display())))?;
        }
    }

    let backend = ZstdBackend::new();
    backend.diff(pred.installer, new_installer, pred.patch_out)?;

    // The client's own path, run here: take the artifact the user has, apply
    // the patch just written, and require the result to be the artifact being
    // published — byte for byte, by digest.
    let scratch = tempfile::Builder::new()
        .prefix("delta-release-roundtrip-")
        .tempdir()
        .map_err(|e| Error::Io(format!("creating a scratch directory: {e}")))?;
    let rebuilt = scratch.path().join("rebuilt.artifact");

    backend.apply(pred.installer, pred.patch_out, &rebuilt)?;

    let actual = FileHash::of_file(&rebuilt)?;
    if actual != expected {
        return Err(Error::Request(format!(
            "the generated patch does not reconstruct {}: applying it to {} produced \
             {}, but the release publishes {}. Refusing to describe a patch that \
             does not work.",
            new_installer.display(),
            pred.installer.display(),
            actual.to_hex(),
            expected.to_hex(),
        )));
    }

    Ok(Patch {
        backend_id: ZstdBackend::ID.to_owned(),
        patch_url: pred.patch_url.to_owned(),
        patch_blake3: FileHash::of_file(pred.patch_out)?.to_hex(),
        patch_size: file_size(pred.patch_out)?,
    })
}

/// Generate the tar layer for one upgrade path, or say why not.
fn build_tar_layer(
    req: &ReleaseRequest<'_>,
    pred: &Predecessor<'_>,
    options: &TarLayerOptions<'_>,
    existing: Option<tauri_updater_delta_core::TarLayer>,
) -> Result<(tauri_updater_delta_core::TarLayer, u64)> {
    if !tar_layer::looks_like_app_tar_gz(req.new_installer)
        || !tar_layer::looks_like_app_tar_gz(pred.installer)
    {
        return Err(Error::Request(
            "the artifacts are not gzipped tarballs".to_owned(),
        ));
    }

    let default_work = tar_layer::default_work_dir(options.patch_out);
    let work_dir = options.work_dir.unwrap_or(&default_work);

    let (layer, summary) = tar_layer::generate(
        &tar_layer::TarLayerRequest {
            from_version: pred.from_version,
            previous_installer: pred.installer,
            new_installer: req.new_installer,
            patch_url: options.patch_url,
            patch_out: options.patch_out,
            work_dir,
            max_tar_bytes: options.max_tar_bytes,
        },
        existing,
    )?;
    let _ = std::fs::remove_dir(work_dir);
    Ok((layer, summary.patch_size))
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)
        .map_err(|e| Error::Io(format!("stat {}: {e}", path.display())))?
        .len())
}

/// Read a manifest from disk, if it exists.
///
/// A missing file is not an error — that is simply the first release.
pub fn load_manifest(path: &Path) -> Result<Option<Manifest>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(Manifest::from_json(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(format!("reading {}: {e}", path.display()))),
    }
}

/// Write a manifest to disk as pretty JSON.
pub fn write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let json = manifest.to_json()?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("creating {}: {e}", parent.display())))?;
        }
    }
    std::fs::write(path, json).map_err(|e| Error::Io(format!("writing {}: {e}", path.display())))
}
