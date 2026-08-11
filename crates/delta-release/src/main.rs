//! `delta-release` — generate a patch and update the release manifest.
//!
//! File in, file out. Nothing here talks to the network, so it can be run
//! locally against two installers exactly as CI runs it against two release
//! artifacts.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tauri_updater_delta_release::signing::SigningKey;
use tauri_updater_delta_release::{
    build_release, load_manifest, write_manifest, ReleaseRequest, Result,
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
    /// Tauri platform identifier, e.g. linux-x86_64.
    #[arg(long)]
    platform: String,

    /// Version being released.
    #[arg(long)]
    version: String,

    /// Version this patch upgrades from.
    #[arg(long)]
    from_version: String,

    /// Installer that users on --from-version already have.
    #[arg(long)]
    previous_installer: PathBuf,

    /// Installer being released.
    #[arg(long)]
    new_installer: PathBuf,

    /// Public URL of the full installer.
    #[arg(long)]
    installer_url: String,

    /// Public URL the patch will be served from.
    #[arg(long)]
    patch_url: String,

    /// Where to write the generated patch.
    #[arg(long)]
    patch_out: PathBuf,

    /// Manifest to create or update.
    #[arg(long, default_value = "manifest.json")]
    manifest: PathBuf,

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
    let key = load_key(args.private_key.as_deref())?;

    let request = ReleaseRequest {
        platform: &args.platform,
        version: &args.version,
        from_version: &args.from_version,
        previous_installer: &args.previous_installer,
        new_installer: &args.new_installer,
        installer_url: &args.installer_url,
        patch_url: &args.patch_url,
        patch_out: &args.patch_out,
        notes: args.notes.as_deref(),
        pub_date: args.pub_date.as_deref(),
    };

    let existing = load_manifest(&args.manifest)?;
    let (manifest, summary) = build_release(&request, &key, existing)?;

    println!(
        "{} -> {} on {}: patch {} bytes, installer {} bytes ({:.2}% of a full download)",
        args.from_version,
        args.version,
        args.platform,
        summary.patch_size,
        summary.installer_size,
        summary.ratio_percent(),
    );
    println!("patch written to {}", args.patch_out.display());

    if args.dry_run {
        println!("--dry-run: manifest not written. It would have been:\n");
        println!("{}", manifest.to_json()?);
    } else {
        write_manifest(&args.manifest, &manifest)?;
        println!("manifest written to {}", args.manifest.display());
    }

    Ok(())
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
