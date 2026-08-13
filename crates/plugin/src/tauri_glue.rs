//! The Tauri seam: registration, and the install handoff over `Update`.
//!
//! Everything Tauri-specific in this crate is here. The flow in [`crate::flow`]
//! has no Tauri dependency, so this file is deliberately thin — if it grows,
//! logic has probably leaked out of the layer that can be tested without an app.

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{Manager, Runtime};
use tauri_plugin_updater::Update;
use tauri_updater_delta_core::{UpdateIdentity, VerifiedArtifact};

use crate::flow::InstallHandoff;
use crate::{Error, Result};

/// Derives the authoritative release identity from a checked `Update`.
///
/// An extension trait rather than a `From` impl because both types are foreign
/// to this crate.
pub trait UpdateExt {
    /// The single description of the release this update refers to.
    fn delta_identity(&self) -> UpdateIdentity;
}

/// This is the only place the delta flow learns what it is installing, and it is
/// the whole of the fix recorded in `docs/DECISIONS.md` #13. Every field comes
/// from the one HTTP response `Updater::check()` already made:
///
/// | Identity field | Source on `Update` | Why not derive it ourselves |
/// | --- | --- | --- |
/// | `current_version` | `current_version` | Same value Tauri's semver gate used |
/// | `version` | `version` | The `checked_target` |
/// | `target` | `target` | Tauri's own `{os}-{arch}` string |
/// | `download_url` | `download_url` | Tauri already ran its platform search |
/// | `signature` | `signature` | Selected by that same search |
/// | `raw_json` | `raw_json` | The document itself, verbatim |
///
/// `download_url` and `signature` matter most. Tauri searches
/// `{os}-{arch}-{installer}` before `{os}-{arch}` and does not report which key
/// won, so reproducing that search here would create a second selection
/// algorithm free to drift from the real one. Consuming its *result* cannot
/// drift.
impl UpdateExt for Update {
    fn delta_identity(&self) -> UpdateIdentity {
        UpdateIdentity::new(
            &self.current_version,
            &self.version,
            &self.target,
            self.download_url.as_str(),
            &self.signature,
            self.raw_json.to_string(),
        )
    }
}

/// Hands a verified artifact to the official updater's own install step.
///
/// This is the entire wrap-don't-replace design in one call: the bytes are
/// bit-identical to what `Update::download` would have produced, so
/// `Update::install` runs exactly as it always does — same installer logic, same
/// platform handling, nothing forked.
///
/// It takes a [`VerifiedArtifact`] because `install` itself verifies nothing.
/// See `docs/DECISIONS.md` #10.
pub struct TauriInstall<'a> {
    update: &'a Update,
}

impl<'a> TauriInstall<'a> {
    /// Wrap an `Update` obtained from `tauri-plugin-updater`.
    pub fn new(update: &'a Update) -> Self {
        Self { update }
    }
}

impl InstallHandoff for TauriInstall<'_> {
    fn install(&self, artifact: &VerifiedArtifact) -> Result<()> {
        self.update
            .install(artifact.as_bytes())
            .map_err(|e| Error::Install(e.to_string()))
    }
}

/// Configuration for the plugin.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the delta-aware manifest is published.
    ///
    /// This is a superset of Tauri's own updater manifest, so it can be the same
    /// URL the official updater is already pointed at.
    ///
    /// **Currently stored and never read.** Before `docs/DECISIONS.md` #13 the
    /// flow fetched this URL itself; it now reads the release document out of
    /// `Update::raw_json`, so nothing consults this value. It is kept — and
    /// still required by [`Builder::build`] — rather than removed, because
    /// changing the registration API belongs to the developer-experience phase.
    /// Recorded here so the next reader does not assume it is load-bearing.
    pub manifest_url: String,
}

/// Builds the plugin.
#[derive(Debug, Default)]
pub struct Builder {
    manifest_url: Option<String>,
}

impl Builder {
    /// Start configuring the plugin.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the manifest URL. Required.
    pub fn manifest_url(mut self, url: impl Into<String>) -> Self {
        self.manifest_url = Some(url.into());
        self
    }

    /// Build the plugin.
    ///
    /// # Errors
    ///
    /// If no manifest URL was set. Failing at registration is deliberate: a
    /// plugin that silently does nothing because it was misconfigured is worse
    /// than an app that refuses to start.
    pub fn build<R: Runtime>(self) -> std::result::Result<TauriPlugin<R>, Error> {
        let manifest_url = self.manifest_url.ok_or_else(|| {
            Error::Manifest("a manifest URL is required: call Builder::manifest_url".to_owned())
        })?;

        Ok(PluginBuilder::new("updater-delta")
            .setup(move |app, _api| {
                app.manage(Config {
                    manifest_url: manifest_url.clone(),
                });
                Ok(())
            })
            .build())
    }
}
