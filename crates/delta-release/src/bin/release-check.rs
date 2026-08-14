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
use tauri_updater_delta_release::{load_manifest, Error, Result};

#[derive(Parser, Debug)]
#[command(
    name = "release-check",
    about = "Verify a generated manifest describes the release being published",
    version
)]
struct Args {
    /// Manifest to check.
    #[arg(long)]
    manifest: PathBuf,

    /// The tag being published, e.g. `v1.2.3`.
    #[arg(long)]
    tag: String,

    /// Application bundle identifier, as in `tauri.conf.json`.
    #[arg(long)]
    app_id: String,

    /// Tauri platform identifier this release publishes.
    #[arg(long)]
    platform: String,

    /// The artifact file about to be uploaded.
    #[arg(long)]
    artifact: PathBuf,

    /// Base64 minisign public key, as in `tauri.conf.json`.
    ///
    /// Either the key itself or a path to a file containing it.
    #[arg(long)]
    pubkey: String,

    /// Permit `http://` URLs. For loopback rehearsals only.
    #[arg(long)]
    allow_insecure_urls: bool,
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

    let manifest = load_manifest(&args.manifest)?.ok_or_else(|| {
        Error::Request(format!(
            "refusing to publish: {} does not exist. A release without an updater \
             document publishes an artifact no client can find.",
            args.manifest.display()
        ))
    })?;

    let pubkey_path = PathBuf::from(&args.pubkey);
    let pubkey = if pubkey_path.is_file() {
        std::fs::read_to_string(&pubkey_path)
            .map_err(|e| Error::Io(format!("reading {}: {e}", pubkey_path.display())))?
            .trim()
            .to_owned()
    } else {
        args.pubkey.clone()
    };

    let report = verify_release(
        &manifest,
        &ReleaseUnderTest {
            tag: &args.tag,
            app_id: &args.app_id,
            platform: &args.platform,
            artifact: &args.artifact,
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
