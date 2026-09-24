//! `delta-release` — generate a patch and update the release manifest.
//!
//! File in, file out. Nothing here talks to the network, so it can be run
//! locally against two installers exactly as CI runs it against two release
//! artifacts.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::version_contract::{check_app_version, AppVersionSources};
use tauri_updater_delta_release::{
    build_release, load_manifest, write_manifest, Predecessor, ReleaseRequest, Result,
    TarLayerOptions,
};

/// Environment variable Tauri uses for the signing key, reused here so a project
/// does not need a second secret.
const KEY_ENV: &str = "TAURI_SIGNING_PRIVATE_KEY";
/// Matching password variable.
const KEY_PASSWORD_ENV: &str = "TAURI_SIGNING_PRIVATE_KEY_PASSWORD";

#[derive(Parser, Debug)]
#[command(
    name = "delta-release",
    about = "Generate a binary delta patch and update the Tauri update manifest",
    version
)]
struct Args {
    /// Tauri platform identifier, e.g. darwin-aarch64.
    #[arg(long)]
    platform: String,

    /// Version being released.
    ///
    /// Named `--target-version` rather than `--version` because clap reserves
    /// the latter for the tool's own version flag. Declaring both made every
    /// invocation of a debug build panic — including `--help`. See
    /// `docs/DECISIONS.md` #20.
    #[arg(long)]
    target_version: String,

    /// Version this patch upgrades from.
    ///
    /// Omit on a first release. All four predecessor flags — this,
    /// `--previous-installer`, `--patch-url` and `--patch-out` — are required
    /// together or not at all, so a release either describes a complete upgrade
    /// path or describes none. See `docs/DECISIONS.md` #32.
    #[arg(
        long,
        requires_all = ["previous_installer", "patch_url", "patch_out"]
    )]
    from_version: Option<String>,

    /// Application bundle identifier, as in `tauri.conf.json`'s `identifier`.
    ///
    /// Bound into the signature's authenticated release identity, so a client
    /// can refuse an artifact belonging to a different application signed with
    /// the same key. Required rather than inferred: reading it out of a bundle
    /// works for exactly one format on one platform.
    ///
    /// Prefer `--app-config`, which reads this from the same file the build
    /// reads and additionally checks that the application's own version is the
    /// one being released.
    #[arg(long, required_unless_present = "app_config")]
    app_id: Option<String>,

    /// The application's `tauri.conf.json`.
    ///
    /// Supplies `--app-id`, and enforces the release-version contract: the
    /// version in that file, the version in the application crate's
    /// `Cargo.toml`, and `--target-version` must all be the same. Without it a
    /// release can sign an artifact under a version the application will
    /// contradict the moment it launches. See
    /// `tauri_updater_delta_release::version_contract`.
    #[arg(long)]
    app_config: Option<PathBuf>,

    /// Installer that users on --from-version already have.
    #[arg(long, requires = "from_version")]
    previous_installer: Option<PathBuf>,

    /// Installer being released.
    #[arg(long)]
    new_installer: PathBuf,

    /// Public URL of the full installer.
    #[arg(long)]
    installer_url: String,

    /// Public URL the patch will be served from.
    #[arg(long, requires = "from_version")]
    patch_url: Option<String>,

    /// Where to write the generated patch.
    #[arg(long, requires = "from_version")]
    patch_out: Option<PathBuf>,

    /// Publish a direct patch only when it is strictly smaller than this
    /// percentage of the full installer.
    ///
    /// Compressed container formats can make an otherwise valid patch almost
    /// as large as a full download. Such a patch adds cache, CPU and failure
    /// cost without delivering a useful saving, so the safe default is to omit
    /// it and leave clients on the ordinary Full path.
    #[arg(
        long,
        default_value_t = 30,
        value_parser = clap::value_parser!(u8).range(1..=100)
    )]
    max_direct_patch_percent: u8,

    /// Fail instead of publishing Full-only when the direct patch misses
    /// `--max-direct-patch-percent`.
    ///
    /// Intended for CI demonstrations which promise an efficient delta. Normal
    /// release jobs should omit this flag and degrade safely to Full.
    #[arg(long, requires = "from_version")]
    require_direct_patch: bool,

    /// Manifest to create or update.
    #[arg(long, default_value = "manifest.json")]
    manifest: PathBuf,

    /// Also write the signature to this file, as `tauri build` does.
    ///
    /// The updater reads signatures out of the manifest, so this is not required
    /// for updates to work. It is published because the `.sig` beside the
    /// artifact is what the rest of the Tauri ecosystem expects to find, and a
    /// release that omits it looks broken to every tool that is not this one.
    #[arg(long)]
    signature_out: Option<PathBuf>,

    /// Release notes.
    #[arg(long)]
    notes: Option<String>,

    /// RFC 3339 publication timestamp.
    #[arg(long)]
    pub_date: Option<String>,

    /// Signing key file. Defaults to the TAURI_SIGNING_PRIVATE_KEY environment
    /// variable, which may hold either the key itself or a path to it.
    #[arg(long)]
    private_key: Option<String>,

    /// Also generate a tar-layer patch, written here.
    ///
    /// Only meaningful for gzipped tarball artifacts such as macOS
    /// `.app.tar.gz`. The direct patch is generated either way, so a release
    /// that cannot produce a tar layer still publishes normally.
    #[arg(long, requires_all = ["tar_patch_url", "from_version"])]
    tar_patch_out: Option<PathBuf>,

    /// Public URL the tar-layer patch will be served from.
    #[arg(long, requires = "tar_patch_out")]
    tar_patch_url: Option<String>,

    /// Fail the release if a tar-layer patch cannot be produced.
    ///
    /// A missing tar layer is invisible in the manifest — the release looks
    /// fine and every client silently does the expensive thing — so a project
    /// that has decided to depend on it wants this on.
    #[arg(long, requires = "tar_patch_out")]
    require_tar_layer: bool,

    /// Largest tar the tar-layer generator will expand, in bytes.
    #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024)]
    max_tar_bytes: u64,

    /// Permit `http://` URLs pointing at loopback, for this repository's
    /// end-to-end harness.
    ///
    /// Production clients refuse a non-HTTPS artifact URL, so a manifest
    /// carrying one is rejected by every client that fetches it. This flag
    /// exists so the loopback harness can still run; it accepts any loopback
    /// address — `127.0.0.0/8`, `::1` and the name `localhost` — and refuses
    /// every other host. `release-check` applies the identical rule.
    #[arg(long)]
    dangerously_allow_loopback_http_urls: bool,

    /// Do everything except write the manifest — generate the patch, sign, and
    /// print what would be published.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args = Args::parse();

    // The version contract, checked before the key is even loaded: a release
    // that signs the wrong version is not recoverable once published, and the
    // cheapest, most informative refusal should come first.
    let app_id = match &args.app_config {
        Some(config) => {
            let sources = AppVersionSources::beside(config);
            let facts = check_app_version(&args.target_version, &sources)?;
            if let Some(explicit) = &args.app_id {
                if explicit != &facts.app_id {
                    return Err(tauri_updater_delta_release::Error::Request(format!(
                        "--app-id is {explicit:?} but {} says the application is {:?}",
                        sources.tauri_conf.display(),
                        facts.app_id,
                    )));
                }
            }
            println!(
                "version contract: {} and {} agree on {}",
                sources.tauri_conf.display(),
                sources.cargo_manifest.display(),
                facts.version,
            );
            facts.app_id
        }
        // clap's `required_unless_present` guarantees one of the two is here.
        None => args
            .app_id
            .clone()
            .expect("clap requires --app-id when --app-config is absent"),
    };

    let key = load_key(args.private_key.as_deref())?;

    let tar_layer = match (&args.tar_patch_out, &args.tar_patch_url) {
        (Some(out), Some(url)) => Some(TarLayerOptions {
            patch_url: url,
            patch_out: out,
            work_dir: None,
            max_tar_bytes: args.max_tar_bytes,
            required: args.require_tar_layer,
        }),
        _ => None,
    };

    // clap's `requires_all` has already established that these are all present
    // or all absent, so the four `zip`s below cannot disagree.
    let predecessor = match (
        &args.from_version,
        &args.previous_installer,
        &args.patch_url,
        &args.patch_out,
    ) {
        (Some(from_version), Some(installer), Some(patch_url), Some(patch_out)) => {
            Some(Predecessor {
                from_version,
                installer,
                patch_url,
                patch_out,
                tar_layer,
            })
        }
        _ => None,
    };

    let request = ReleaseRequest {
        platform: &args.platform,
        version: &args.target_version,
        new_installer: &args.new_installer,
        installer_url: &args.installer_url,
        notes: args.notes.as_deref(),
        pub_date: args.pub_date.as_deref(),
        app_id: &app_id,
        predecessor,
        allow_insecure_urls: args.dangerously_allow_loopback_http_urls,
    };

    let existing = load_manifest(&args.manifest)?;
    let (mut manifest, summary) = build_release(&request, &key, existing)?;

    let mut direct_patch_omitted = None;
    let oversized_direct_patch = match (
        args.from_version.as_deref(),
        summary.patch_size,
        args.patch_out.as_deref(),
    ) {
        (Some(from_version), Some(patch_size), Some(patch_out))
            if !patch_is_below_percent_limit(
                patch_size,
                summary.installer_size,
                args.max_direct_patch_percent,
            ) =>
        {
            Some((from_version, patch_out))
        }
        _ => None,
    };
    if let Some((from_version, patch_out)) = oversized_direct_patch {
        let percent = summary.ratio_percent().unwrap_or(0.0);
        let reason = format!(
            "direct patch is {percent:.2}% of Full; it must be strictly below {}%",
            args.max_direct_patch_percent
        );

        // A generated-but-unpublished file is dangerous in a release
        // directory: a later glob can upload it even though the manifest
        // correctly omitted it. Delete it before reporting or returning.
        match std::fs::remove_file(patch_out) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(tauri_updater_delta_release::Error::Io(format!(
                    "removing oversized patch {}: {error}",
                    patch_out.display()
                )))
            }
        }

        if let Some(entry) = manifest
            .delta
            .as_mut()
            .and_then(|delta| delta.platforms.get_mut(args.platform.as_str()))
        {
            entry.patches.remove(from_version);
        }
        manifest.validate()?;

        if args.require_direct_patch {
            return Err(tauri_updater_delta_release::Error::Request(format!(
                "{reason}; refusing the release because --require-direct-patch was set"
            )));
        }
        direct_patch_omitted = Some(reason);
    }

    if let Some(path) = &args.signature_out {
        // Taken from the manifest rather than re-signed: minisign includes
        // randomness, so signing twice produces two different valid signatures
        // and the `.sig` file would not be the one the manifest published.
        let signature = &manifest
            .platforms
            .get(&args.platform)
            .expect("build_release always writes the platform it was asked for")
            .signature;
        std::fs::write(path, signature).map_err(|e| {
            tauri_updater_delta_release::Error::Io(format!("writing {}: {e}", path.display()))
        })?;
        println!("signature written to {}", path.display());
    }

    match (
        summary.patch_size,
        summary.ratio_percent(),
        direct_patch_omitted.as_deref(),
    ) {
        (Some(_), Some(_), Some(reason)) => {
            eprintln!("warning: {reason}");
            eprintln!("warning: oversized direct patch omitted; clients will download Full");
        }
        (Some(patch), Some(percent), None) => {
            println!(
                "{} -> {} on {}: patch {} bytes, installer {} bytes ({:.2}% of a full download)",
                args.from_version.as_deref().unwrap_or("?"),
                args.target_version,
                args.platform,
                patch,
                summary.installer_size,
                percent,
            );
            if let Some(path) = &args.patch_out {
                println!("patch written to {}", path.display());
            }
        }
        // A first release, or any release with no predecessor supplied. Said
        // plainly, because this used to be the case that produced nothing at
        // all -- see docs/DECISIONS.md #32.
        (None, None, _) => {
            println!(
                "{} on {}: no predecessor, so no patches. Publishing a complete \
                 full-download release: installer {} bytes, signed, with an \
                 authenticated release identity.",
                args.target_version, args.platform, summary.installer_size,
            );
        }
        _ => unreachable!("patch size and ratio are always present together"),
    }

    match (&summary.tar_patch_size, &summary.tar_layer_skipped) {
        (Some(size), _) => {
            let percent = if summary.installer_size == 0 {
                0.0
            } else {
                *size as f64 / summary.installer_size as f64 * 100.0
            };
            println!(
                "tar-layer patch {size} bytes ({percent:.2}% of a full download), \
                 round-tripped to the exact published artifact"
            );
            if let Some(path) = &args.tar_patch_out {
                println!("tar-layer patch written to {}", path.display());
            }
        }
        // Loud on stderr rather than quiet on stdout: a missing tar layer looks
        // exactly like a successful release in every other respect.
        (None, Some(reason)) => {
            eprintln!("warning: no tar layer published: {reason}");
            if direct_patch_omitted.is_some() {
                eprintln!(
                    "warning: the direct patch also missed its size limit; clients will use Full."
                );
            } else {
                eprintln!(
                    "warning: clients will use the direct patch. Pass --require-tar-layer to fail."
                );
            }
        }
        (None, None) => {}
    }

    if args.dry_run {
        println!("--dry-run: manifest not written. It would have been:\n");
        println!("{}", manifest.to_json()?);
    } else {
        write_manifest(&args.manifest, &manifest)?;
        println!("manifest written to {}", args.manifest.display());
    }

    Ok(())
}

/// Whether a direct patch earns publication under an exclusive percentage
/// limit. Integer cross-multiplication avoids rounding a 30.004% patch down to
/// the displayed 30.00%, and `u128` keeps the multiplication safe for `u64`
/// artifact sizes.
fn patch_is_below_percent_limit(patch: u64, full: u64, limit: u8) -> bool {
    (patch as u128) * 100 < (full as u128) * (limit as u128)
}

/// Resolve the signing key from `--private-key` or the Tauri environment
/// variables, accepting either an inline key or a path to one.
fn load_key(explicit: Option<&str>) -> Result<SigningKey> {
    let password = std::env::var(KEY_PASSWORD_ENV).ok();

    let source = match explicit {
        Some(value) => value.to_owned(),
        None => std::env::var(KEY_ENV).map_err(|_| {
            tauri_updater_delta_release::Error::Key(format!(
                "no signing key: pass --private-key or set {KEY_ENV}"
            ))
        })?,
    };

    let path = PathBuf::from(&source);
    if path.is_file() {
        SigningKey::from_file(&path, password)
    } else {
        SigningKey::from_str(&source, password)
    }
}

#[cfg(test)]
mod patch_limit_tests {
    use super::patch_is_below_percent_limit;

    #[test]
    fn the_limit_is_strict_and_does_not_round() {
        assert!(patch_is_below_percent_limit(29, 100, 30));
        assert!(!patch_is_below_percent_limit(30, 100, 30));
        assert!(!patch_is_below_percent_limit(30_004, 100_000, 30));
    }

    #[test]
    fn ratio_math_cannot_overflow_at_u64_sizes() {
        assert!(!patch_is_below_percent_limit(u64::MAX, u64::MAX, 30));
    }
}
