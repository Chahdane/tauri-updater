//! The release a tag names, and the version actually compiled into the app.
//!
//! # The conflation this closes
//!
//! This workspace publishes two different things on two different clocks:
//!
//! | Track | Version lives in | Example |
//! | --- | --- | --- |
//! | The three public crates | the workspace `[workspace.package] version` | `0.1.0` |
//! | The demonstration application | `examples/desktop-app/tauri.conf.json` and its own `Cargo.toml` | `1.0.0` |
//!
//! The release workflow derived `--target-version` from the git tag and never
//! looked at either. So tagging `v0.1.0` — the crate release — built an
//! application whose own version was `1.0.0`, then signed and published it under
//! the authenticated release identity `0.1.0`. Every cryptographic check would
//! have passed. The artifact would simply have been a lie: a client on `1.0.0`
//! offered `0.1.0` sees a downgrade, and a client that installed it would come
//! back up reporting `1.0.0`, so the cache would discard the PENDING entry and
//! no later update could ever take a delta.
//!
//! `release-check` could not catch it. It compares the tag, the manifest, the
//! signature's identity and the bytes — all of which agree with each other,
//! because they were all derived from the same `--target-version`. The one thing
//! nobody compared was the version *inside* the thing being built.
//!
//! # What is checked, and why here rather than in the bundle
//!
//! The honest question is "what version will this build report about itself?",
//! and the honest answer is read from the two files the build reads:
//!
//! - `tauri.conf.json`'s `version`, which is what `tauri::generate_context!`
//!   compiles in and what the bundler names the artifact after; and
//! - the application crate's `Cargo.toml` `version`, which is what
//!   `PackageInfo::version` reports at runtime — and therefore what the cache's
//!   launch reconciliation compares a staged PENDING entry against.
//!
//! Those two must agree with each other and with the tag. Reading the built
//! bundle instead would be one bundle format on one platform, and would answer
//! the question after the build rather than before it.
//!
//! A version-less application crate is refused outright rather than resolved:
//! `version.workspace = true` on the demo app is the conflation itself, spelled
//! in a manifest.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The two files a Tauri application's own version is read from.
#[derive(Debug, Clone)]
pub struct AppVersionSources {
    /// The application's `tauri.conf.json`.
    pub tauri_conf: PathBuf,
    /// The application crate's `Cargo.toml`.
    ///
    /// Defaults to the `Cargo.toml` beside `tauri_conf`, which is where a Tauri
    /// application keeps it.
    pub cargo_manifest: PathBuf,
}

impl AppVersionSources {
    /// Derive both paths from a `tauri.conf.json`.
    pub fn beside(tauri_conf: impl Into<PathBuf>) -> Self {
        let tauri_conf = tauri_conf.into();
        let cargo_manifest = tauri_conf
            .parent()
            .unwrap_or(Path::new("."))
            .join("Cargo.toml");
        Self {
            tauri_conf,
            cargo_manifest,
        }
    }
}

/// What the application says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFacts {
    /// `tauri.conf.json`'s `identifier`, the application bundle id.
    pub app_id: String,
    /// The version both files agree on.
    pub version: String,
    /// `plugins.updater.pubkey`, when the configuration carries one.
    pub pubkey: Option<String>,
}

/// Strip a release tag down to the version it names.
///
/// Accepts `1.2.3`, `v1.2.3` and `app-v1.2.3`. The last exists because the two
/// release tracks now have separate tag namespaces: `v*` publishes the crates,
/// `app-v*` publishes the demonstration application. One tag meaning both is
/// exactly the conflation this module exists to refuse.
pub fn version_from_tag(tag: &str) -> &str {
    tag.strip_prefix("app-v")
        .or_else(|| tag.strip_prefix('v'))
        .unwrap_or(tag)
}

/// Read the application's own facts, requiring both files to agree on a version.
pub fn read_app_facts(sources: &AppVersionSources) -> Result<AppFacts> {
    let conf_text = std::fs::read_to_string(&sources.tauri_conf)
        .map_err(|e| Error::Io(format!("reading {}: {e}", sources.tauri_conf.display())))?;
    let conf: serde_json::Value = serde_json::from_str(&conf_text).map_err(|e| {
        Error::Request(format!(
            "{} is not valid JSON: {e}",
            sources.tauri_conf.display()
        ))
    })?;

    let app_id = conf
        .get("identifier")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::Request(format!(
                "{} has no `identifier`; the authenticated release identity \
                 binds the artifact to it, so it cannot be guessed",
                sources.tauri_conf.display()
            ))
        })?
        .to_owned();

    let conf_version = conf
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::Request(format!(
                "{} has no `version`. Tauri would fall back to the crate version, \
                 so the application's version would be decided by a file this \
                 check is not reading.",
                sources.tauri_conf.display()
            ))
        })?
        .to_owned();

    let cargo_version = read_package_version(&sources.cargo_manifest)?;

    if conf_version != cargo_version {
        return Err(Error::Request(format!(
            "the application disagrees with itself about its version: {} says {:?} \
             and {} says {:?}. The first is what the bundler compiles in; the \
             second is what the running process reports, and what the update \
             cache compares a staged artifact against on launch.",
            sources.tauri_conf.display(),
            conf_version,
            sources.cargo_manifest.display(),
            cargo_version,
        )));
    }

    let pubkey = conf
        .get("plugins")
        .and_then(|v| v.get("updater"))
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(ToOwned::to_owned);

    Ok(AppFacts {
        app_id,
        version: conf_version,
        pubkey,
    })
}

/// Read the application's facts and require its version to be the one `tag`
/// names.
///
/// This is the invariant. A release tag that does not name the version being
/// built publishes an artifact whose signed identity contradicts what the
/// application will report about itself the moment it launches.
pub fn check_app_version(tag: &str, sources: &AppVersionSources) -> Result<AppFacts> {
    let facts = read_app_facts(sources)?;
    let expected = version_from_tag(tag);
    if facts.version != expected {
        return Err(Error::Request(format!(
            "refusing to publish: the tag {tag} names version {expected}, but the \
             application in {} is version {}. A crate-release tag must not \
             publish a differently versioned application: the signature would \
             authenticate an identity the application contradicts on its first \
             launch, and every client would see a downgrade.",
            sources.tauri_conf.display(),
            facts.version,
        )));
    }
    Ok(facts)
}

/// Read `[package] version` from a Cargo manifest, refusing an inherited one.
///
/// Deliberately a small hand-rolled read rather than a TOML dependency: the
/// question is whether this file states a version of its own, and
/// `version.workspace = true` has to be an *error* rather than something to
/// resolve. A parser that resolved it would answer with the crate release's
/// version, which is the mistake.
fn read_package_version(manifest: &Path) -> Result<String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| Error::Io(format!("reading {}: {e}", manifest.display())))?;

    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("version") {
            let rest = rest.trim();
            if let Some(value) = rest.strip_prefix('=') {
                return Ok(value.trim().trim_matches('"').to_owned());
            }
            if rest.starts_with(".workspace") {
                return Err(Error::Request(format!(
                    "{} inherits its version from the workspace. The demonstration \
                     application and the published crates are separate release \
                     tracks; an application that inherits the crate version makes \
                     one tag mean both.",
                    manifest.display()
                )));
            }
        }
    }

    Err(Error::Request(format!(
        "{} has no [package] version",
        manifest.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_app(dir: &Path, conf_version: &str, cargo_version: &str) -> AppVersionSources {
        std::fs::write(
            dir.join("tauri.conf.json"),
            format!(
                r#"{{"productName":"Demo","version":"{conf_version}",
                     "identifier":"dev.example.demo",
                     "plugins":{{"updater":{{"pubkey":"a-key"}}}}}}"#
            ),
        )
        .expect("write conf");
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"demo\"\nversion = \"{cargo_version}\"\n"),
        )
        .expect("write manifest");
        AppVersionSources::beside(dir.join("tauri.conf.json"))
    }

    #[test]
    fn a_tag_naming_the_built_version_is_accepted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sources = write_app(dir.path(), "1.0.0", "1.0.0");

        for tag in ["1.0.0", "v1.0.0", "app-v1.0.0"] {
            let facts = check_app_version(tag, &sources)
                .unwrap_or_else(|e| panic!("{tag} should be accepted: {e}"));
            assert_eq!(facts.version, "1.0.0");
            assert_eq!(facts.app_id, "dev.example.demo");
            assert_eq!(facts.pubkey.as_deref(), Some("a-key"));
        }
    }

    /// The exact scenario from the audit: the crate release tag, against the
    /// application as it actually stands.
    #[test]
    fn the_crate_release_tag_does_not_publish_the_demo_app() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sources = write_app(dir.path(), "1.0.0", "1.0.0");

        let err = check_app_version("v0.1.0", &sources)
            .expect_err("v0.1.0 must not publish a 1.0.0 application");
        let message = err.to_string();
        assert!(message.contains("names version 0.1.0"), "{message}");
        assert!(message.contains("is version 1.0.0"), "{message}");
    }

    #[test]
    fn the_two_files_must_agree_with_each_other() {
        let dir = tempfile::tempdir().expect("temp dir");
        // What the bundler compiles in, and what the running process reports,
        // taken from two files that had drifted.
        let sources = write_app(dir.path(), "1.0.1", "1.0.0");

        let err = check_app_version("v1.0.1", &sources)
            .expect_err("a build that disagrees with itself is not publishable");
        assert!(
            err.to_string().contains("disagrees with itself"),
            "got: {err}"
        );
    }

    #[test]
    fn an_application_inheriting_the_workspace_version_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_app(dir.path(), "1.0.0", "1.0.0");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion.workspace = true\n",
        )
        .expect("write manifest");
        let sources = AppVersionSources::beside(dir.path().join("tauri.conf.json"));

        let err = check_app_version("v1.0.0", &sources)
            .expect_err("an inherited version makes one tag mean both tracks");
        assert!(
            err.to_string().contains("inherits its version"),
            "got: {err}"
        );
    }

    #[test]
    fn a_configuration_without_a_version_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_app(dir.path(), "1.0.0", "1.0.0");
        std::fs::write(
            dir.path().join("tauri.conf.json"),
            r#"{"productName":"Demo","identifier":"dev.example.demo"}"#,
        )
        .expect("write conf");
        let sources = AppVersionSources::beside(dir.path().join("tauri.conf.json"));

        let err = check_app_version("v1.0.0", &sources)
            .expect_err("a config with no version decides it somewhere this check cannot see");
        assert!(err.to_string().contains("has no `version`"), "got: {err}");
    }

    #[test]
    fn the_two_tag_namespaces_are_distinguished() {
        assert_eq!(version_from_tag("v0.1.0"), "0.1.0");
        assert_eq!(version_from_tag("app-v1.0.0"), "1.0.0");
        assert_eq!(version_from_tag("1.0.0"), "1.0.0");
        // `app-v` is stripped whole, not as `app-` then `v`.
        assert_eq!(version_from_tag("app-v0.0.0-hosted-a"), "0.0.0-hosted-a");
    }
}
