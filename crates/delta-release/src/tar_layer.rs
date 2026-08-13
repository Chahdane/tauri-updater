//! Generating tar-layer patches for compressed bundle artifacts.
//!
//! # What this adds to a release
//!
//! An ordinary release publishes a patch between two `.app.tar.gz` files. That
//! saves almost nothing, because gzip output shifts wholesale when its input
//! changes (`docs/DECISIONS.md` #15). This publishes a second patch, between the
//! *tars inside* them, and the metadata a client needs to use it: which tar the
//! patch expects, which compressed artifact that tar came out of, and which tar
//! it produces.
//!
//! Both patches ship. A client that cannot rebuild the compressed artifact — an
//! older build, a different platform, a future representation it does not
//! implement — reads the direct patch and behaves exactly as it does today.
//!
//! # The self-check, and why it is not optional
//!
//! [`generate`] applies its own patch before returning, decompresses,
//! recompresses, and requires the result to be byte-identical to the artifact it
//! was given. A tar-layer release has three places to be wrong that a direct
//! patch does not:
//!
//! 1. the patch might not reproduce the tar,
//! 2. the tar might not be what the client's decompressor produces, and
//! 3. **the recompression recipe might not reproduce the published artifact on
//!    this release's build** — which depends on the bundler's dependency graph,
//!    not on anything in this tool.
//!
//! (3) is the dangerous one, because it is invisible at generation time and
//! fails on every client. A release that publishes a tar layer its own
//! recompression cannot reproduce would have every client do the work and fall
//! back, which is strictly worse than publishing no tar layer at all. So the
//! generator runs the client's exact path once and refuses to emit metadata it
//! could not consume itself.
//!
//! That closes Audit #2 blocker B7 for this path specifically, and only for this
//! path: the direct-patch generator still does not round-trip its own output.

use std::path::{Path, PathBuf};

use tauri_updater_delta_core::backend::{PatchBackend, ZstdBackend};
use tauri_updater_delta_core::manifest::{
    TarLayer, TarPatch, RECOMPRESSION_TAURI_APP_TAR_GZ_V1, REPRESENTATION_APP_TAR_GZ_V1,
};
use tauri_updater_delta_core::recompress::{decompress_bounded, recompress_app_tar_gz};
use tauri_updater_delta_core::FileHash;

use crate::{Error, Result};

/// One tar-layer patch to publish.
#[derive(Debug, Clone)]
pub struct TarLayerRequest<'a> {
    /// Version this patch upgrades from.
    pub from_version: &'a str,
    /// The compressed artifact users on `from_version` already have.
    pub previous_installer: &'a Path,
    /// The compressed artifact being released.
    pub new_installer: &'a Path,
    /// Where the tar patch will be downloadable.
    pub patch_url: &'a str,
    /// Where to write the generated tar patch.
    pub patch_out: &'a Path,
    /// Scratch directory for the intermediate tars and the round-trip check.
    pub work_dir: &'a Path,
    /// Largest tar this run will expand, as a guard against a corrupt artifact.
    pub max_tar_bytes: u64,
}

/// What generating a tar-layer patch produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarLayerSummary {
    /// Size of the generated tar patch.
    pub patch_size: u64,
    /// Size of the tar it reconstructs.
    pub target_tar_size: u64,
    /// Size of the compressed artifact that tar is inside.
    pub target_installer_size: u64,
    /// Size of the direct patch published alongside it, if one was.
    pub direct_patch_size: Option<u64>,
}

impl TarLayerSummary {
    /// Tar patch size as a percentage of a full compressed download.
    pub fn ratio_percent(&self) -> f64 {
        if self.target_installer_size == 0 {
            return 0.0;
        }
        self.patch_size as f64 / self.target_installer_size as f64 * 100.0
    }
}

/// Generate a tar-layer patch and the metadata describing it.
///
/// `existing` is the tar layer already built for this platform, so several
/// upgrade paths can be added to one release by calling this repeatedly.
///
/// Fails rather than emitting metadata whose round-trip did not hold. Every
/// error here means *do not publish a tar layer for this pair* — the direct
/// patch and the full download are unaffected.
pub fn generate(
    req: &TarLayerRequest<'_>,
    existing: Option<TarLayer>,
) -> Result<(TarLayer, TarLayerSummary)> {
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

    std::fs::create_dir_all(req.work_dir)
        .map_err(|e| Error::Io(format!("creating {}: {e}", req.work_dir.display())))?;
    if let Some(parent) = req.patch_out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("creating {}: {e}", parent.display())))?;
        }
    }

    // The exact tars, obtained the same bounded way the client obtains them.
    let base_tar = req.work_dir.join("base.tar");
    let target_tar = req.work_dir.join("target.tar");
    let base_tar_size = decompress_bounded(req.previous_installer, &base_tar, req.max_tar_bytes)?;
    let target_tar_size = decompress_bounded(req.new_installer, &target_tar, req.max_tar_bytes)?;

    let base_tar_blake3 = FileHash::of_file(&base_tar)?;
    let target_tar_blake3 = FileHash::of_file(&target_tar)?;
    if base_tar_blake3 == target_tar_blake3 {
        return Err(Error::Request(format!(
            "the tars inside {} and {} are identical; there is nothing to patch",
            req.previous_installer.display(),
            req.new_installer.display()
        )));
    }

    ZstdBackend::new().diff(&base_tar, &target_tar, req.patch_out)?;
    let patch_blake3 = FileHash::of_file(req.patch_out)?;
    let patch_size = file_size(req.patch_out)?;

    // The client's whole path, run here: apply, check the tar, recompress,
    // check the artifact. See the module docs for why this is not optional.
    round_trip(req, &base_tar, &target_tar_blake3, target_tar_size)?;

    let patch = TarPatch {
        backend_id: ZstdBackend::ID.to_owned(),
        patch_url: req.patch_url.to_owned(),
        patch_blake3: patch_blake3.to_hex(),
        patch_size,
        base_installer_blake3: FileHash::of_file(req.previous_installer)?.to_hex(),
        base_installer_size: file_size(req.previous_installer)?,
        base_tar_blake3: base_tar_blake3.to_hex(),
        base_tar_size,
    };

    // A layer carried over from an earlier call describes the same release
    // only if it names the same target tar. If it does not, its patches
    // reconstruct something that is no longer being published, and keeping them
    // would send clients to rebuild the wrong artifact -- caught by the digest
    // gate, but only after the download.
    let existing = existing.filter(|layer| layer.target_tar_blake3 == target_tar_blake3.to_hex());

    let mut layer = existing.unwrap_or(TarLayer {
        representation: REPRESENTATION_APP_TAR_GZ_V1.to_owned(),
        recompression: RECOMPRESSION_TAURI_APP_TAR_GZ_V1.to_owned(),
        target_tar_blake3: target_tar_blake3.to_hex(),
        target_tar_size,
        patches: Default::default(),
    });

    // Keep the target description in step explicitly rather than trusting that
    // whatever was there described this release — the same reasoning as the
    // direct layer's re-assignment of its target fields.
    layer.representation = REPRESENTATION_APP_TAR_GZ_V1.to_owned();
    layer.recompression = RECOMPRESSION_TAURI_APP_TAR_GZ_V1.to_owned();
    layer.target_tar_blake3 = target_tar_blake3.to_hex();
    layer.target_tar_size = target_tar_size;
    layer.patches.insert(req.from_version.to_owned(), patch);

    let _ = std::fs::remove_file(&base_tar);
    let _ = std::fs::remove_file(&target_tar);

    Ok((
        layer,
        TarLayerSummary {
            patch_size,
            target_tar_size,
            target_installer_size: file_size(req.new_installer)?,
            direct_patch_size: None,
        },
    ))
}

/// Apply the generated patch and rebuild the compressed artifact from it,
/// exactly as a client would, and require both results to be exact.
fn round_trip(
    req: &TarLayerRequest<'_>,
    base_tar: &Path,
    expected_tar: &FileHash,
    expected_tar_size: u64,
) -> Result<()> {
    let rebuilt_tar = req.work_dir.join("round-trip.tar");
    ZstdBackend::new()
        .with_expected_output_bytes(expected_tar_size)
        .apply(base_tar, req.patch_out, &rebuilt_tar)?;

    let actual = FileHash::of_file(&rebuilt_tar)?;
    if &actual != expected_tar {
        return Err(Error::Request(format!(
            "the generated tar patch does not reproduce the target tar \
             (expected {}, got {}); no tar layer published",
            expected_tar.to_hex(),
            actual.to_hex()
        )));
    }

    let rebuilt_installer = req.work_dir.join("round-trip.app.tar.gz");
    recompress_app_tar_gz(&rebuilt_tar, &rebuilt_installer)?;

    let expected_installer = FileHash::of_file(req.new_installer)?;
    let actual_installer = FileHash::of_file(&rebuilt_installer)?;
    if actual_installer != expected_installer {
        return Err(Error::Request(format!(
            "recompressing the reconstructed tar does not reproduce {} \
             (expected {}, got {}). The published artifact was not built by a \
             toolchain the `{}` recipe reproduces, so a tar layer would make \
             every client do the work and fall back. Publishing the direct \
             patch only.",
            req.new_installer.display(),
            expected_installer.to_hex(),
            actual_installer.to_hex(),
            RECOMPRESSION_TAURI_APP_TAR_GZ_V1,
        )));
    }

    let _ = std::fs::remove_file(&rebuilt_tar);
    let _ = std::fs::remove_file(&rebuilt_installer);
    Ok(())
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)
        .map_err(|e| Error::Io(format!("stat {}: {e}", path.display())))?
        .len())
}

/// Whether an artifact looks like something the tar layer can describe.
///
/// A cheap pre-check for callers deciding whether to attempt generation at all:
/// the file starts with gzip's magic and its name ends in `.tar.gz`. It proves
/// nothing — [`generate`] does the proving — and exists so a caller can skip a
/// `.msi` without treating "not a tarball" as a release failure.
pub fn looks_like_app_tar_gz(path: &Path) -> bool {
    use std::io::Read as _;
    if !path
        .to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(".tar.gz")
    {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic).is_ok() && magic == [0x1f, 0x8b]
}

/// A conventional scratch directory beside the patch being written.
pub fn default_work_dir(patch_out: &Path) -> PathBuf {
    patch_out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .join(".delta-tar-work")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Build a real `.app.tar.gz` the way `tauri-bundler` does.
    fn bundle(dir: &Path, name: &str, binary: &[u8]) -> PathBuf {
        let root = dir.join(format!("{name}-src"));
        std::fs::create_dir_all(root.join("Contents/MacOS")).expect("mkdir");
        std::fs::write(root.join("Contents/Info.plist"), b"<plist/>").expect("write");
        let mut f = std::fs::File::create(root.join("Contents/MacOS/app")).expect("create");
        f.write_all(binary).expect("write");
        drop(f);

        let out = dir.join(format!("{name}.app.tar.gz"));
        let encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&out).expect("create"),
            flate2::Compression::default(),
        );
        let mut builder = tar::Builder::new(encoder);
        builder.follow_symlinks(false);
        builder.append_dir_all("Bundle.app", &root).expect("append");
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

    struct Fixture {
        dir: tempfile::TempDir,
        old: PathBuf,
        new: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        // Mostly shared, so the tar patch is a real delta rather than a
        // recompression of the whole binary.
        let mut old_bin = binary(1, 200_000);
        let mut new_bin = old_bin.clone();
        new_bin[100_000..100_064].copy_from_slice(&binary(7, 64));
        old_bin.extend_from_slice(b"old tail");
        new_bin.extend_from_slice(b"new tail, longer");

        let old = bundle(dir.path(), "old", &old_bin);
        let new = bundle(dir.path(), "new", &new_bin);
        Fixture { dir, old, new }
    }

    fn request<'a>(f: &'a Fixture, patch: &'a Path, work: &'a Path) -> TarLayerRequest<'a> {
        TarLayerRequest {
            from_version: "1.0.0",
            previous_installer: &f.old,
            new_installer: &f.new,
            patch_url: "https://example.com/1.0.0-to-1.0.1.tar.zst",
            patch_out: patch,
            work_dir: work,
            max_tar_bytes: 64 * 1024 * 1024,
        }
    }

    #[test]
    fn generates_metadata_that_describes_the_real_artifacts() {
        let f = fixture();
        let patch = f.dir.path().join("tar.zst");
        let work = f.dir.path().join("work");
        let (layer, summary) = generate(&request(&f, &patch, &work), None).expect("generate");

        assert_eq!(layer.representation, REPRESENTATION_APP_TAR_GZ_V1);
        assert_eq!(layer.recompression, RECOMPRESSION_TAURI_APP_TAR_GZ_V1);
        assert!(layer.is_supported());

        let entry = &layer.patches["1.0.0"];
        assert_eq!(
            entry.base_installer_blake3,
            FileHash::of_file(&f.old).expect("hash").to_hex()
        );
        assert_eq!(
            entry.base_installer_size,
            std::fs::metadata(&f.old).expect("stat").len()
        );
        assert_eq!(
            entry.patch_blake3,
            FileHash::of_file(&patch).expect("hash").to_hex()
        );
        assert_eq!(entry.patch_size, summary.patch_size);
        assert_eq!(summary.target_tar_size, layer.target_tar_size);

        // And the declared tars really are the tars.
        let out = f.dir.path().join("check.tar");
        decompress_bounded(&f.old, &out, 64 * 1024 * 1024).expect("decompress");
        assert_eq!(
            FileHash::of_file(&out).expect("hash").to_hex(),
            entry.base_tar_blake3
        );
        decompress_bounded(&f.new, &out, 64 * 1024 * 1024).expect("decompress");
        assert_eq!(
            FileHash::of_file(&out).expect("hash").to_hex(),
            layer.target_tar_blake3
        );
    }

    #[test]
    fn the_tar_patch_is_much_smaller_than_the_compressed_one() {
        // The reason the tar layer exists, asserted rather than assumed. Both
        // patches are generated from the same pair by the same backend.
        let f = fixture();
        let tar_patch = f.dir.path().join("tar.zst");
        let work = f.dir.path().join("work");
        let (_, summary) = generate(&request(&f, &tar_patch, &work), None).expect("generate");

        let direct_patch = f.dir.path().join("direct.zst");
        ZstdBackend::new()
            .diff(&f.old, &f.new, &direct_patch)
            .expect("direct diff");
        let direct = std::fs::metadata(&direct_patch).expect("stat").len();

        assert!(
            summary.patch_size * 4 < direct,
            "the tar patch ({} bytes) should be far smaller than the direct one ({direct} bytes)",
            summary.patch_size
        );
    }

    #[test]
    fn a_layer_describing_another_target_does_not_carry_its_patches_over() {
        let f = fixture();
        let work = f.dir.path().join("work");
        let first_patch = f.dir.path().join("a.zst");
        let (mut stale, _) = generate(&request(&f, &first_patch, &work), None).expect("first");
        stale.target_tar_blake3 = FileHash::of_bytes(b"some earlier release").to_hex();

        let second_patch = f.dir.path().join("b.zst");
        let mut second = request(&f, &second_patch, &work);
        second.from_version = "0.9.0";
        let (layer, _) = generate(&second, Some(stale)).expect("second");

        assert_eq!(
            layer.patches.keys().collect::<Vec<_>>(),
            vec!["0.9.0"],
            "patches that rebuilt a different target must not be carried forward"
        );
    }

    #[test]
    fn several_upgrade_paths_accumulate_in_one_layer() {
        let f = fixture();
        let work = f.dir.path().join("work");
        let first_patch = f.dir.path().join("a.zst");
        let (layer, _) = generate(&request(&f, &first_patch, &work), None).expect("first");

        let second_patch = f.dir.path().join("b.zst");
        let mut second = request(&f, &second_patch, &work);
        second.from_version = "0.9.0";
        let (layer, _) = generate(&second, Some(layer)).expect("second");

        assert_eq!(layer.patches.len(), 2);
        assert!(layer.patches.contains_key("1.0.0"));
        assert!(layer.patches.contains_key("0.9.0"));
    }

    #[test]
    fn refuses_to_publish_when_recompression_would_not_reproduce_the_artifact() {
        // The check that matters. The "published" artifact here was compressed
        // by a recipe the client does not implement, so a tar layer describing
        // it would make every client fall back. This is the case that is
        // invisible without a round-trip.
        let f = fixture();
        let tar = f.dir.path().join("target.tar");
        decompress_bounded(&f.new, &tar, 64 * 1024 * 1024).expect("decompress");

        // Same tar, compressed in one shot rather than through tar::Builder's
        // write boundaries -- a real artifact, and not one the recipe rebuilds.
        let foreign = f.dir.path().join("foreign.app.tar.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&foreign).expect("create"),
            flate2::Compression::default(),
        );
        encoder
            .write_all(&std::fs::read(&tar).expect("read tar"))
            .expect("write");
        encoder.finish().expect("finish");

        let work = f.dir.path().join("work");
        let patch = f.dir.path().join("p.zst");
        let mut req = request(&f, &patch, &work);
        req.new_installer = &foreign;

        let err = generate(&req, None).expect_err("must refuse to publish");
        let message = err.to_string();
        assert!(
            message.contains("does not reproduce"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains(RECOMPRESSION_TAURI_APP_TAR_GZ_V1),
            "the error must name the recipe that failed: {message}"
        );
    }

    #[test]
    fn refuses_a_pair_whose_tars_are_identical() {
        let f = fixture();
        let work = f.dir.path().join("work");
        let patch = f.dir.path().join("p.zst");
        let mut req = request(&f, &patch, &work);
        req.previous_installer = &f.new;
        let err = generate(&req, None).expect_err("must refuse");
        assert!(err.to_string().contains("nothing to patch"), "{err}");
    }

    #[test]
    fn refuses_an_artifact_larger_than_the_local_ceiling() {
        let f = fixture();
        let work = f.dir.path().join("work");
        let patch = f.dir.path().join("p.zst");
        let mut req = request(&f, &patch, &work);
        req.max_tar_bytes = 1024;
        assert!(generate(&req, None).is_err());
    }

    #[test]
    fn a_missing_installer_is_a_request_error() {
        let f = fixture();
        let absent = f.dir.path().join("nope.app.tar.gz");
        let work = f.dir.path().join("work");
        let patch = f.dir.path().join("p.zst");
        let mut req = request(&f, &patch, &work);
        req.previous_installer = &absent;
        assert!(matches!(generate(&req, None), Err(Error::Request(_))));
    }

    #[test]
    fn recognises_tarballs_without_committing_to_them() {
        let f = fixture();
        assert!(looks_like_app_tar_gz(&f.old));
        let msi = f.dir.path().join("installer.msi");
        std::fs::write(&msi, b"not a tarball").expect("write");
        assert!(!looks_like_app_tar_gz(&msi));

        // Right name, wrong contents.
        let liar = f.dir.path().join("liar.app.tar.gz");
        std::fs::write(&liar, b"still not a tarball").expect("write");
        assert!(!looks_like_app_tar_gz(&liar));
    }
}
