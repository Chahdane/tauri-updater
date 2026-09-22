//! `release-check` — the gate between a generated manifest and a public release.
//!
//! Separate from `delta-release` on purpose. The generator writes the manifest;
//! this reads it back as a stranger would, hashes the artifact that is actually
//! about to be uploaded, verifies the signature under the configured public key,
//! and checks that the story the document tells is the release being published.
//!
//! Two binaries rather than a flag, because a checker that shares state with the
//! thing it checks can agree with it for the wrong reason. This one is handed a
//! file path and a key and knows nothing else.
//!
//! The previous version of this gate was eleven lines of `python3` heredoc
//! inside the release workflow, which could not be run locally, could not be
//! tested, and checked four of the ten things that matter.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tauri_updater_delta_release::verify::{verify_release, ReleaseUnderTest};
use tauri_updater_delta_release::version_contract::{check_app_version, AppVersionSources};
use tauri_updater_delta_release::{load_manifest, Error, Result};

#[derive(Parser, Debug)]
#[command(
    name = "release-check",
    about = "Verify a generated manifest describes the release being published",
    version
)]
struct Args {
    /// Manifest to check.
    #[arg(long, required_unless_present = "only_version_contract")]
    manifest: Option<PathBuf>,

    /// The tag being published, e.g. `app-v1.2.3`.
    #[arg(long)]
    tag: String,

    /// Application bundle identifier, as in `tauri.conf.json`.
    ///
    /// Prefer `--app-config`, which reads this from the file the build read.
    #[arg(long, required_unless_present = "app_config")]
    app_id: Option<String>,

    /// The application's `tauri.conf.json`.
    ///
    /// Supplies `--app-id` and `--pubkey`, and adds the one check this gate
    /// could not previously make: that the version compiled into the
    /// application is the version the tag publishes. Everything else here is
    /// derived from `--target-version`, so the manifest, the signature's
    /// identity and the bytes all agree with each other whether or not the
    /// application does. See `tauri_updater_delta_release::version_contract`.
    #[arg(long)]
    app_config: Option<PathBuf>,

    /// Tauri platform identifier this release publishes.
    #[arg(long, required_unless_present = "only_version_contract")]
    platform: Option<String>,

    /// The artifact file about to be uploaded.
    #[arg(long, required_unless_present = "only_version_contract")]
    artifact: Option<PathBuf>,

    /// Base64 minisign public key, as in `tauri.conf.json`.
    ///
    /// Either the key itself or a path to a file containing it. Supplied by
    /// `--app-config` when that is given.
    #[arg(long, required_unless_present = "app_config")]
    pubkey: Option<String>,

    /// Permit `http://` URLs pointing at loopback, for local rehearsals.
    ///
    /// Narrowed to loopback and nothing else, which is the same rule
    /// `delta-release` applies when generating the manifest. A checker that
    /// accepted more than the generator could not catch a generator bug.
    #[arg(long)]
    allow_insecure_urls: bool,

    /// Check only that the application's version is the one the tag names, then
    /// exit.
    ///
    /// For running the contract *before* a twenty-minute build rather than
    /// after it. Deliberately the same code path as the full gate, so the fast
    /// pre-check and the publish gate cannot come to different conclusions --
    /// which is the class of bug that made the URL policy a finding of its own.
    #[arg(long, requires = "app_config")]
    only_version_contract: bool,
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

    // The version contract. Checked first, and separately, because it is the
    // one claim every other check here is blind to: the manifest, the
    // signature's identity and the artifact digest were all derived from one
    // `--target-version`, so they agree with each other by construction. What
    // the application will report about itself on its first launch is decided
    // somewhere else entirely.
    let app_facts = match &args.app_config {
        Some(config) => {
            let facts = check_app_version(&args.tag, &AppVersionSources::beside(config))?;
            println!(
                "version contract: the application is {}, which is what {} publishes",
                facts.version, args.tag
            );
            Some(facts)
        }
        None => None,
    };

    if args.only_version_contract {
        return Ok(());
    }

    let manifest_path = args
        .manifest
        .clone()
        .expect("clap requires --manifest unless --only-version-contract");
    let manifest = load_manifest(&manifest_path)?.ok_or_else(|| {
        Error::Request(format!(
            "refusing to publish: {} does not exist. A release without an updater \
             document publishes an artifact no client can find.",
            manifest_path.display()
        ))
    })?;

    let app_id = resolve(
        args.app_id.clone(),
        app_facts.as_ref().map(|f| f.app_id.clone()),
        "--app-id",
    )?;
    let pubkey = resolve(
        args.pubkey.clone(),
        app_facts.as_ref().and_then(|f| f.pubkey.clone()),
        "--pubkey",
    )?;
    // Either the key itself or a path to a file holding it.
    let pubkey_path = PathBuf::from(&pubkey);
    let pubkey = if pubkey_path.is_file() {
        std::fs::read_to_string(&pubkey_path)
            .map_err(|e| Error::Io(format!("reading {}: {e}", pubkey_path.display())))?
            .trim()
            .to_owned()
    } else {
        pubkey
    };

    let platform = args
        .platform
        .clone()
        .expect("clap requires --platform unless --only-version-contract");
    let artifact = args
        .artifact
        .clone()
        .expect("clap requires --artifact unless --only-version-contract");

    let report = verify_release(
        &manifest,
        &ReleaseUnderTest {
            tag: &args.tag,
            app_id: &app_id,
            platform: &platform,
            artifact: &artifact,
            pubkey: &pubkey,
            allow_insecure_urls: args.allow_insecure_urls,
        },
    )?;

    println!(
        "release {} on {} checks out:",
        report.version, report.platform
    );
    println!(
        "  artifact   {} ({} bytes)",
        report.artifact_blake3, report.artifact_size
    );
    println!("  identity   {}", report.identity);
    println!("  signature  verifies against the artifact under the configured key");
    if report.direct_patch_from.is_empty() {
        println!("  direct     none published");
    } else {
        println!("  direct     from {}", report.direct_patch_from.join(", "));
    }
    if report.tar_patch_from.is_empty() {
        println!("  tar layer  none published");
    } else {
        println!("  tar layer  from {}", report.tar_patch_from.join(", "));
    }

    Ok(())
}

/// Take the explicit flag when given, otherwise the value `--app-config`
/// supplied, and refuse when neither produced one.
///
/// An explicit flag wins so an operator can override a configuration file, and
/// disagreement is reported rather than silently resolved.
fn resolve(explicit: Option<String>, from_config: Option<String>, flag: &str) -> Result<String> {
    match (explicit, from_config) {
        (Some(explicit), Some(from_config)) if explicit != from_config => Err(Error::Request(
            format!("{flag} is {explicit:?} but --app-config says {from_config:?}"),
        )),
        (Some(value), _) | (None, Some(value)) => Ok(value),
        (None, None) => Err(Error::Request(format!(
            "{flag} is required: --app-config did not supply one"
        ))),
    }
}
