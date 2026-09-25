//! `delta-release` — generate a patch and update the release manifest.
//!
//! File in, file out. Nothing here talks to the network, so it can be run
//! locally against release artifacts exactly as CI runs it.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::version_contract::{check_app_version, AppVersionSources};
use tauri_updater_delta_release::{
    add_compressed_full, build_release, load_manifest, write_manifest, Predecessor, ReleaseRequest,
    Result, TarLayerOptions,
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

    /// Version a patch upgrades from. Repeat for every supported predecessor.
    ///
    /// Omit on a first release. Repeat all four predecessor flags — this,
    /// `--previous-installer`, `--patch-url` and `--patch-out` — the same number
    /// of times in matching order. See `docs/DECISIONS.md` #7 and #32.
    #[arg(
        long,
        requires_all = ["previous_installer", "patch_url", "patch_out"]
    )]
    from_version: Vec<String>,

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

    /// Installer that users on the corresponding --from-version already have.
    #[arg(long, requires = "from_version")]
    previous_installer: Vec<PathBuf>,

    /// Installer being released.
    #[arg(long)]
    new_installer: PathBuf,

    /// Public URL of the full installer.
    #[arg(long)]
    installer_url: String,

    /// Public URL the corresponding patch will be served from.
    #[arg(long, requires = "from_version")]
    patch_url: Vec<String>,

    /// Where to write the corresponding generated patch.
    #[arg(long, requires = "from_version")]
    patch_out: Vec<PathBuf>,

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

    /// Fail instead of omitting a predecessor when any direct patch misses
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

    /// Also generate tar-layer patches, written here in predecessor order.
    ///
    /// Only meaningful for gzipped tarball artifacts such as macOS
    /// `.app.tar.gz`. The direct patch is generated either way, so a release
    /// that cannot produce a tar layer still publishes normally.
    #[arg(long, requires_all = ["tar_patch_url", "from_version"])]
    tar_patch_out: Vec<PathBuf>,

    /// Public URL the corresponding tar-layer patch will be served from.
    #[arg(long, requires = "tar_patch_out")]
    tar_patch_url: Vec<String>,

    /// Fail the release if any requested tar-layer patch cannot be produced.
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

    /// Also publish a zstd-compressed copy of the full installer, written here.
    ///
    /// Plugin clients that must download the whole installer fetch this
    /// instead and rebuild the exact installer from it; stock Tauri clients keep
    /// using --installer-url. Round-tripped before it is described, and omitted
    /// if it is not smaller. Most useful with uncompressed NSIS installers. See
    /// docs/DECISIONS.md #40.
    #[arg(long, requires = "compressed_full_url")]
    compressed_full_out: Option<PathBuf>,

    /// Public URL the compressed full copy will be served from.
    #[arg(long, requires = "compressed_full_out")]
    compressed_full_url: Option<String>,

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

fn validate_predecessor_argument_counts(args: &Args) -> Result<()> {
    let predecessor_count = args.from_version.len();
    for (flag, count) in [
        ("--previous-installer", args.previous_installer.len()),
        ("--patch-url", args.patch_url.len()),
        ("--patch-out", args.patch_out.len()),
    ] {
        if count != predecessor_count {
            return Err(tauri_updater_delta_release::Error::Request(format!(
                "received {predecessor_count} --from-version value(s), but {count} {flag} value(s); repeat all four predecessor flags in matching order"
            )));
        }
    }

    if args.tar_patch_out.len() != args.tar_patch_url.len() {
        return Err(tauri_updater_delta_release::Error::Request(format!(
            "received {} --tar-patch-out value(s), but {} --tar-patch-url value(s)",
            args.tar_patch_out.len(),
            args.tar_patch_url.len()
        )));
    }
    if !args.tar_patch_out.is_empty() && args.tar_patch_out.len() != predecessor_count {
        return Err(tauri_updater_delta_release::Error::Request(format!(
            "received {predecessor_count} predecessor(s), but {} tar-layer patch pair(s); provide one tar-layer pair per predecessor or none",
            args.tar_patch_out.len()
        )));
    }
    Ok(())
}

fn run() -> Result<()> {
    let args = Args::parse();
    validate_predecessor_argument_counts(&args)?;

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

    let predecessors = (0..args.from_version.len())
        .map(|index| Predecessor {
            from_version: &args.from_version[index],
            installer: &args.previous_installer[index],
            patch_url: &args.patch_url[index],
            patch_out: &args.patch_out[index],
            tar_layer: (!args.tar_patch_out.is_empty()).then(|| TarLayerOptions {
                patch_url: &args.tar_patch_url[index],
                patch_out: &args.tar_patch_out[index],
                work_dir: None,
                max_tar_bytes: args.max_tar_bytes,
                required: args.require_tar_layer,
            }),
        })
        .collect::<Vec<_>>();

    let request = ReleaseRequest {
        platform: &args.platform,
        version: &args.target_version,
        new_installer: &args.new_installer,
        installer_url: &args.installer_url,
        notes: args.notes.as_deref(),
        pub_date: args.pub_date.as_deref(),
        app_id: &app_id,
        predecessors: &predecessors,
        allow_insecure_urls: args.dangerously_allow_loopback_http_urls,
    };

    let existing = load_manifest(&args.manifest)?;
    let (mut manifest, summary) = build_release(&request, &key, existing)?;

    let mut direct_patch_omissions = Vec::new();
    for (index, patch) in summary.patches.iter().enumerate() {
        if patch_is_below_percent_limit(
            patch.patch_size,
            summary.installer_size,
            args.max_direct_patch_percent,
        ) {
            continue;
        }

        let patch_out = &args.patch_out[index];
        let percent = patch.ratio_percent(summary.installer_size);
        let reason = format!(
            "{} -> {} direct patch is {percent:.2}% of Full; it must be strictly below {}%",
            patch.from_version, args.target_version, args.max_direct_patch_percent
        );

        // A generated-but-unpublished file is dangerous in a release
        // directory: a later glob can upload it even though the manifest
        // correctly omitted it. Delete every oversized predecessor patch before
        // reporting or returning.
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
            entry.patches.remove(&patch.from_version);
        }
        direct_patch_omissions.push((patch.from_version.clone(), reason));
    }
    manifest.validate()?;

    if args.require_direct_patch && !direct_patch_omissions.is_empty() {
        let reasons = direct_patch_omissions
            .iter()
            .map(|(_, reason)| reason.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(tauri_updater_delta_release::Error::Request(format!(
            "{reasons}; refusing the release because --require-direct-patch was set"
        )));
    }

    if let (Some(out), Some(url)) = (&args.compressed_full_out, &args.compressed_full_url) {
        match add_compressed_full(
            &mut manifest,
            &args.platform,
            &args.new_installer,
            url,
            out,
            args.dangerously_allow_loopback_http_urls,
        )? {
            Some(full) => println!(
                "compressed full copy {} bytes ({:.2}% of the {}-byte installer), round-tripped, written to {}",
                full.size,
                full.size as f64 / summary.installer_size.max(1) as f64 * 100.0,
                summary.installer_size,
                out.display()
            ),
            None => eprintln!(
                "warning: the compressed full copy was not smaller than the installer; not published"
            ),
        }
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

    if summary.patches.is_empty() {
        // A first release, or any release with no predecessor supplied. Said
        // plainly, because this used to be the case that produced nothing at
        // all -- see docs/DECISIONS.md #32.
        println!(
            "{} on {}: no predecessor, so no patches. Publishing a complete \
             full-download release: installer {} bytes, signed, with an \
             authenticated release identity.",
            args.target_version, args.platform, summary.installer_size,
        );
    }

    for (index, patch) in summary.patches.iter().enumerate() {
        let direct_omission = direct_patch_omissions
            .iter()
            .find(|(from_version, _)| from_version == &patch.from_version)
            .map(|(_, reason)| reason);
        if let Some(reason) = direct_omission {
            eprintln!("warning: {reason}");
            eprintln!(
                "warning: oversized direct patch from {} omitted",
                patch.from_version
            );
        } else {
            println!(
                "{} -> {} on {}: direct patch {} bytes, installer {} bytes ({:.2}% of a full download), round-tripped",
                patch.from_version,
                args.target_version,
                args.platform,
                patch.patch_size,
                summary.installer_size,
                patch.ratio_percent(summary.installer_size),
            );
            println!("patch written to {}", args.patch_out[index].display());
        }

        match (&patch.tar_patch_size, &patch.tar_layer_skipped) {
            (Some(size), _) => {
                let percent = if summary.installer_size == 0 {
                    0.0
                } else {
                    *size as f64 / summary.installer_size as f64 * 100.0
                };
                println!(
                    "{} -> {} tar-layer patch {size} bytes ({percent:.2}% of a full download), round-tripped to the exact published artifact",
                    patch.from_version, args.target_version
                );
                println!(
                    "tar-layer patch written to {}",
                    args.tar_patch_out[index].display()
                );
            }
            // Loud on stderr rather than quiet on stdout: a missing tar layer
            // looks exactly like a successful release in every other respect.
            (None, Some(reason)) => {
                eprintln!(
                    "warning: no tar layer from {} published: {reason}",
                    patch.from_version
                );
                if direct_omission.is_some() {
                    eprintln!(
                        "warning: its direct patch also missed the size limit; clients on {} will use Full.",
                        patch.from_version
                    );
                } else {
                    eprintln!(
                        "warning: clients on {} will use the direct patch. Pass --require-tar-layer to fail.",
                        patch.from_version
                    );
                }
            }
            (None, None) if direct_omission.is_some() => {
                eprintln!(
                    "warning: clients on {} have no published patch and will use Full",
                    patch.from_version
                );
            }
            (None, None) => {}
        }
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

    #[test]
    fn the_limit_decision_is_independent_for_each_predecessor() {
        let publish = [5, 30, 29, 90].map(|patch| patch_is_below_percent_limit(patch, 100, 30));
        assert_eq!(publish, [true, false, true, false]);
    }
}
