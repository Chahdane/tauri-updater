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

/// One upgrade path to publish: from an installed version to this release.
#[derive(Debug, Clone)]
pub struct ReleaseRequest<'a> {
    /// Tauri platform identifier, e.g. `"linux-x86_64"`.
    pub platform: &'a str,
    /// Version being released.
    pub version: &'a str,
    /// Version this patch upgrades from.
    pub from_version: &'a str,
    /// Installer users on `from_version` already have.
    pub previous_installer: &'a Path,
    /// Installer being released.
    pub new_installer: &'a Path,
    /// Where the full installer will be downloadable.
    pub installer_url: &'a str,
    /// Where the patch will be downloadable.
    pub patch_url: &'a str,
    /// Where to write the generated patch.
    pub patch_out: &'a Path,
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
    /// `identifier`; the release workflow reads it with `jq`.
    pub app_id: &'a str,

    /// Also publish a tar-layer patch, when the artifacts support one.
    ///
    /// `None` reproduces the pre-tar-layer behaviour exactly.
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
    /// Size of the generated patch in bytes.
    pub patch_size: u64,
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
    pub fn ratio_percent(&self) -> f64 {
        if self.installer_size == 0 {
            return 0.0;
        }
        self.patch_size as f64 / self.installer_size as f64 * 100.0
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
    if req.from_version == req.version {
        return Err(Error::Request(format!(
            "cannot patch {} to itself",
            req.version
        )));
    }
    for (label, path) in [
        ("previous installer", req.previous_installer),
        ("new installer", req.new_installer),
    ] {
        if !path.is_file() {
            return Err(Error::Request(format!(
                "{label} {} does not exist",
                path.display()
            )));
        }
    }

    if let Some(parent) = req.patch_out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("creating {}: {e}", parent.display())))?;
        }
    }

    ZstdBackend::new().diff(req.previous_installer, req.new_installer, req.patch_out)?;

    let installer_digest = FileHash::of_file(req.new_installer)?;
    let installer_size = file_size(req.new_installer)?;
    let patch_digest = FileHash::of_file(req.patch_out)?;
    let patch_size = file_size(req.patch_out)?;

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

    let patch = Patch {
        backend_id: ZstdBackend::ID.to_owned(),
        patch_url: req.patch_url.to_owned(),
        patch_blake3: patch_digest.to_hex(),
        patch_size,
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
    entry.patches.insert(req.from_version.to_owned(), patch);

    // The tar layer is strictly additive: if anything about it fails, the entry
    // above is already complete and the release publishes without it.
    let mut tar_patch_size = None;
    let mut tar_layer_skipped = None;
    if let Some(options) = &req.tar_layer {
        match build_tar_layer(req, options, entry.tar_layer.take()) {
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

/// Generate the tar layer for one upgrade path, or say why not.
fn build_tar_layer(
    req: &ReleaseRequest<'_>,
    options: &TarLayerOptions<'_>,
    existing: Option<tauri_updater_delta_core::TarLayer>,
) -> Result<(tauri_updater_delta_core::TarLayer, u64)> {
    if !tar_layer::looks_like_app_tar_gz(req.new_installer)
        || !tar_layer::looks_like_app_tar_gz(req.previous_installer)
    {
        return Err(Error::Request(
            "the artifacts are not gzipped tarballs".to_owned(),
        ));
    }

    let default_work = tar_layer::default_work_dir(options.patch_out);
    let work_dir = options.work_dir.unwrap_or(&default_work);

    let (layer, summary) = tar_layer::generate(
        &tar_layer::TarLayerRequest {
            from_version: req.from_version,
            previous_installer: req.previous_installer,
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
