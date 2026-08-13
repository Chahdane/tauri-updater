//! The Tauri seam: registration, and the install handoff over `Update`.
//!
//! Everything Tauri-specific in this crate is here. The flow in [`crate::flow`]
//! has no Tauri dependency, so this file is deliberately thin — if it grows,
//! logic has probably leaked out of the layer that can be tested without an app.

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri_plugin_updater::Update;
use tauri_updater_delta_core::{UpdateIdentity, VerifiedArtifact};

use crate::flow::{run_update, Context, InstallHandoff, Outcome};
use crate::{Error, Result};

/// Derives the authoritative release identity from a checked `Update`.
///
/// An extension trait rather than a `From` impl because both types are foreign
/// to this crate.
pub trait UpdateExt {
    /// Begin a delta update from this checked update.
    ///
    /// The **only** way to obtain an [`UpdateSession`], and therefore the only
    /// way into the shipping update flow.
    fn delta_session(&self) -> UpdateSession<'_>;
}

/// One update, from the check that produced it to the install that ends it.
///
/// # What this type is for
///
/// Two audit findings had the same shape: the public API let a caller assemble
/// an update out of parts that never belonged together.
///
/// - `UpdateIdentity::new` was public, so any caller could fabricate a version,
///   URL and signature and feed them to the flow as though Tauri had checked
///   them.
/// - `TauriInstall::new` took any `&Update`, so the identity used for planning
///   and verification could come from one checked update while the installation
///   went through a different one.
///
/// Neither was fixed by hiding a constructor, because the *entry point* still
/// accepted the dangerous pairing. What fixes it is removing the pairing: this
/// type holds the checked `Update` and derives the identity from it, and
/// [`install`](UpdateSession::install) installs through **that same `Update`**.
/// There is no seam to get wrong because there are no longer two things to
/// align.
///
/// # The honest limit
///
/// The guarantee is that **the shipping plugin API cannot express the unsafe
/// pairing**. It is not that no Rust program can: a determined developer can
/// depend on `tauri-updater-delta-core` directly and drive the engine by hand.
/// That crate exists to be driven by hand — it is how the flow is tested without
/// an app — and pretending otherwise would be a stronger claim than the code
/// supports.
pub struct UpdateSession<'a> {
    update: &'a Update,
    identity: UpdateIdentity,
}

impl<'a> UpdateSession<'a> {
    /// Version this update installs, as Tauri's own check resolved it.
    pub fn version(&self) -> &str {
        self.identity.version()
    }

    /// Version currently installed, as Tauri determined it.
    pub fn current_version(&self) -> &str {
        self.identity.current_version()
    }

    /// Take the cheapest safe path to this release and install it.
    ///
    /// The identity is the one derived from `self.update`, and the installer is
    /// `self.update`. They cannot disagree.
    pub fn install(
        &self,
        ctx: &Context<'_>,
        fetch: &dyn tauri_updater_delta_core::client::Fetch,
    ) -> crate::Result<Outcome> {
        run_update(&self.identity, ctx, fetch, &TauriInstall::new(self.update))
    }
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
    fn delta_session(&self) -> UpdateSession<'_> {
        UpdateSession {
            update: self,
            identity: identity_of(self),
        }
    }
}

fn identity_of(update: &Update) -> UpdateIdentity {
    UpdateIdentity::new(
        &update.current_version,
        &update.version,
        &update.target,
        update.download_url.as_str(),
        &update.signature,
        update.raw_json.to_string(),
    )
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
pub(crate) struct TauriInstall<'a> {
    update: &'a Update,
}

impl<'a> TauriInstall<'a> {
    /// Wrap an `Update` obtained from `tauri-plugin-updater`.
    pub(crate) fn new(update: &'a Update) -> Self {
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

/// Builds the plugin.
#[derive(Debug, Default)]
pub struct Builder;

impl Builder {
    /// Start configuring the plugin.
    pub fn new() -> Self {
        Self
    }

    /// Build the plugin.
    ///
    /// # Why this takes no configuration
    ///
    /// It used to require a manifest URL. Since `docs/DECISIONS.md` #13 the flow
    /// reads the release document out of `Update::raw_json` — the response
    /// Tauri's own check already made — so nothing consulted that value. It was
    /// left in place for a while and documented as unused, which is the worst of
    /// both worlds: a required, security-adjacent knob that does nothing, on a
    /// plugin whose whole argument is that there is only one source of truth.
    ///
    /// Point Tauri's updater at your manifest as you already do. This plugin
    /// reads the document that check returns.
    pub fn build<R: Runtime>(self) -> std::result::Result<TauriPlugin<R>, Error> {
        Ok(PluginBuilder::new("updater-delta").build())
    }
}
