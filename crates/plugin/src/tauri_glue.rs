//! The small public Tauri seam.
//!
//! A checked update, its authenticated identity, and the installer that consumes
//! it stay in one opaque value. Cache state, transport, paths, and verification
//! are derived or managed by the plugin rather than assembled by applications.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderValue, ACCEPT};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_updater::{Update as TauriUpdate, UpdaterExt as TauriUpdaterExt};
use tauri_updater_delta_core::cache::{ArtifactCache, CacheLimits, Namespace, Reconciliation};
use tauri_updater_delta_core::manifest::{
    RECOMPRESSION_TAURI_APP_TAR_GZ_V1, REPRESENTATION_APP_TAR_GZ_V1,
};
use tauri_updater_delta_core::release_identity::current_platform;
use tauri_updater_delta_core::{FileHash, Limits, UpdateIdentity, VerifiedArtifact};

use crate::flow::{
    run_update_detailed, Context, FlowPhase, InstallHandoff, Outcome as FlowOutcome,
};
use crate::http::{
    HttpFetchBuilder, DEFAULT_CONNECT_TIMEOUT, DEFAULT_MAX_REDIRECTS, DEFAULT_MAX_RESPONSE_BYTES,
    DEFAULT_REQUEST_TIMEOUT,
};
use crate::{Diagnostic, Error, Outcome, ProgressEvent, Result};

type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync + 'static>;

#[derive(Clone)]
struct RuntimeConfig {
    app_id: String,
    pubkey: String,
    cache: Option<Arc<ArtifactCache>>,
    work_dir: PathBuf,
    limits: Limits,
    transport: HttpFetchBuilder,
    diagnostics: Vec<Diagnostic>,
    #[cfg(feature = "test-support")]
    endpoint_override: Option<String>,
    #[cfg(feature = "test-support")]
    base_override: Option<PathBuf>,
}

struct PluginState {
    config: Arc<RuntimeConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPaths {
    cache_dir: PathBuf,
    work_dir: PathBuf,
}

impl ResolvedPaths {
    fn derive(
        app_cache_dir: &Path,
        cache_override: Option<PathBuf>,
        work_override: Option<PathBuf>,
    ) -> Self {
        let root = app_cache_dir.join("updater-delta");
        let cache_dir = cache_override.unwrap_or_else(|| root.join("artifacts"));
        // A broken custom cache is only an optimisation loss. Keep transaction
        // storage independent so it cannot make the Full path unusable too.
        let work_dir = work_override.unwrap_or_else(|| root.join("transactions"));
        Self {
            cache_dir,
            work_dir,
        }
    }
}

/// Builds the delta updater plugin with safe defaults.
///
/// `Builder::new().build()` derives its cache and transaction directories from
/// Tauri's per-application cache directory, reads the application identifier and
/// updater public key from `tauri.conf.json`, applies local resource ceilings,
/// and reconciles the artifact cache on launch.
///
/// Most applications should not call any configuration method. The overrides
/// exist for unusually large artifacts, custom storage policy, and tests.
#[derive(Clone)]
pub struct Builder {
    limits: Limits,
    cache_limits: CacheLimits,
    cache_dir: Option<PathBuf>,
    work_dir: Option<PathBuf>,
    max_response_bytes: u64,
    connect_timeout: Duration,
    request_timeout: Duration,
    max_redirects: usize,
    #[cfg(feature = "test-support")]
    insecure: bool,
    #[cfg(feature = "test-support")]
    endpoint_override: Option<String>,
    #[cfg(feature = "test-support")]
    base_override: Option<PathBuf>,
}

impl std::fmt::Debug for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("limits", &self.limits)
            .field("cache_limits", &self.cache_limits)
            .field("cache_dir", &self.cache_dir)
            .field("work_dir", &self.work_dir)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_redirects", &self.max_redirects)
            .finish_non_exhaustive()
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            cache_limits: CacheLimits::default(),
            cache_dir: None,
            work_dir: None,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            #[cfg(feature = "test-support")]
            insecure: false,
            #[cfg(feature = "test-support")]
            endpoint_override: None,
            #[cfg(feature = "test-support")]
            base_override: None,
        }
    }
}

impl Builder {
    /// Start with the safe default policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the ceilings applied to server-declared reconstruction sizes.
    ///
    /// Raise these only when a legitimate release exceeds the defaults. Server
    /// metadata can lower the effective bound but can never raise this policy.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Override the independent ceilings applied when reusing the local cache.
    ///
    /// Cache files are untrusted and are re-hashed and signature-verified on
    /// every reuse. These limits remain separate from [`Self::limits`] because
    /// expanding a local gzip and reconstructing a server-described tar are two
    /// different resource decisions.
    pub fn cache_limits(mut self, limits: CacheLimits) -> Self {
        self.cache_limits = limits;
        self
    }

    /// Store cached official artifacts under `path`.
    ///
    /// By default the plugin uses `<app-cache>/updater-delta/artifacts`. An
    /// override is normally useful only for tests or an application with an
    /// established cache-location policy. It does not move transaction storage.
    pub fn cache_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(path.into());
        self
    }

    /// Store per-update transaction workspaces under `path`.
    ///
    /// By default this is `<app-cache>/updater-delta/transactions`, alongside
    /// rather than inside the artifact cache. Every update still receives a
    /// private random workspace inside it.
    pub fn work_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.work_dir = Some(path.into());
        self
    }

    /// Override the largest HTTP response body accepted by the plugin.
    pub fn max_response_bytes(mut self, bytes: u64) -> Self {
        self.max_response_bytes = bytes;
        self
    }

    /// Override the connection timeout used for artifact and patch downloads.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Override the whole-request timeout used for artifact and patch downloads.
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Override the maximum number of redirects followed by a download.
    pub fn max_redirects(mut self, redirects: usize) -> Self {
        self.max_redirects = redirects;
        self
    }

    /// Allow plain HTTP in this repository's explicitly feature-gated harness.
    ///
    /// This method does not exist unless the non-default `test-support` feature
    /// is enabled. HTTPS-to-HTTP redirects remain forbidden even then.
    #[cfg(feature = "test-support")]
    pub fn dangerous_insecure_transport_protocol_for_tests(mut self, allow: bool) -> Self {
        self.insecure = allow;
        self
    }

    /// Override the official updater endpoint in the feature-gated harness.
    ///
    /// Normal applications configure updater endpoints in `tauri.conf.json`.
    #[cfg(feature = "test-support")]
    pub fn endpoint_for_tests(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint_override = Some(endpoint.into());
        self
    }

    /// Supply the direct-delta base used by this repository's legacy harness.
    ///
    /// Normal applications do not possess the exact official artifact they are
    /// currently running and should rely on the managed cache's TarDelta path.
    /// This method is absent unless the non-default `test-support` feature is
    /// enabled.
    #[cfg(feature = "test-support")]
    pub fn direct_base_artifact_for_tests(mut self, path: impl Into<PathBuf>) -> Self {
        self.base_override = Some(path.into());
        self
    }

    /// Build the plugin.
    ///
    /// Registration itself has no fallible user configuration. At application
    /// setup the plugin validates that Tauri's updater public key exists and
    /// derives safe per-application paths. A missing key or path is reported as
    /// an application setup error rather than deferred until installation.
    pub fn build<R: Runtime>(self) -> TauriPlugin<R> {
        PluginBuilder::new("updater-delta")
            .setup(move |app, _api| {
                let config = RuntimeConfig::from_app(app, &self)?;
                if !app.manage(PluginState {
                    config: Arc::new(config),
                }) {
                    return Err(Box::new(Error::Configuration(
                        "the delta updater plugin was registered more than once".to_owned(),
                    )));
                }
                Ok(())
            })
            .build()
    }
}

impl RuntimeConfig {
    fn from_app<R: Runtime>(app: &AppHandle<R>, builder: &Builder) -> Result<Self> {
        let pubkey = configured_pubkey(app)?;
        let app_id = app.config().identifier.clone();
        let app_cache_dir = app
            .path()
            .app_cache_dir()
            .map_err(|e| Error::Configuration(format!("resolving the app cache directory: {e}")))?;
        let paths = ResolvedPaths::derive(
            &app_cache_dir,
            builder.cache_dir.clone(),
            builder.work_dir.clone(),
        );

        let namespace = Namespace {
            bundle_id: app_id.clone(),
            platform: current_platform(),
            arch: std::env::consts::ARCH.to_owned(),
            pubkey_fingerprint: FileHash::of_bytes(pubkey.as_bytes()).to_hex(),
            representation: REPRESENTATION_APP_TAR_GZ_V1.to_owned(),
            recompression: RECOMPRESSION_TAURI_APP_TAR_GZ_V1.to_owned(),
        };

        let mut diagnostics = Vec::new();
        let cache = match ArtifactCache::open(&paths.cache_dir, namespace, builder.cache_limits) {
            Ok(cache) => match cache.reconcile(&app.package_info().version.to_string()) {
                Ok(Reconciliation::Promoted { version }) => {
                    log::debug!("promoted delta cache artifact {version} to active");
                    Some(Arc::new(cache))
                }
                Ok(Reconciliation::DiscardedPending { staged, running }) => {
                    log::debug!(
                        "discarded delta cache artifact {staged}; running version is {running}"
                    );
                    Some(Arc::new(cache))
                }
                Ok(Reconciliation::NothingStaged | Reconciliation::NamespaceReset) => {
                    Some(Arc::new(cache))
                }
                Err(error) => {
                    let diagnostic = Diagnostic::CacheUnavailable {
                        message: format!("cache reconciliation failed: {error}"),
                    };
                    log::warn!("{diagnostic}");
                    diagnostics.push(diagnostic);
                    None
                }
            },
            Err(error) => {
                let diagnostic = Diagnostic::CacheUnavailable {
                    message: format!("cache could not be opened: {error}"),
                };
                log::warn!("{diagnostic}");
                diagnostics.push(diagnostic);
                None
            }
        };

        let transport = HttpFetchBuilder::default()
            .max_response_bytes(builder.max_response_bytes)
            .connect_timeout(builder.connect_timeout)
            .request_timeout(builder.request_timeout)
            .max_redirects(builder.max_redirects);
        #[cfg(feature = "test-support")]
        let transport = transport.dangerous_insecure_transport_protocol(builder.insecure);

        Ok(Self {
            app_id,
            pubkey,
            cache,
            work_dir: paths.work_dir,
            limits: builder.limits,
            transport,
            diagnostics,
            #[cfg(feature = "test-support")]
            endpoint_override: builder.endpoint_override.clone(),
            #[cfg(feature = "test-support")]
            base_override: builder.base_override.clone(),
        })
    }
}

fn configured_pubkey<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|value| value.get("pubkey"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            Error::Configuration("plugins.updater.pubkey is required in tauri.conf.json".to_owned())
        })
}

/// Extension trait for accessing the registered delta updater from any Tauri
/// manager, including [`tauri::App`] and [`tauri::AppHandle`].
pub trait DeltaUpdaterExt<R: Runtime> {
    /// Get the high-level updater handle.
    ///
    /// The official `tauri-plugin-updater` and this plugin must both be
    /// registered before this method is used.
    fn delta_updater(&self) -> DeltaUpdater<R>;
}

impl<R: Runtime, T: Manager<R>> DeltaUpdaterExt<R> for T {
    fn delta_updater(&self) -> DeltaUpdater<R> {
        DeltaUpdater {
            app: self.app_handle().clone(),
            config: Arc::clone(&self.state::<PluginState>().config),
        }
    }
}

/// A cloneable high-level handle for checking and installing updates.
///
/// A check is always delegated to the registered official updater. This plugin
/// consumes the `Update` returned by that one request; it never fetches a second
/// release document.
#[derive(Clone)]
pub struct DeltaUpdater<R: Runtime> {
    app: AppHandle<R>,
    config: Arc<RuntimeConfig>,
}

impl<R: Runtime> DeltaUpdater<R> {
    /// Check once through Tauri's authoritative updater.
    ///
    /// `Ok(None)` means no newer release was offered. `Ok(Some(update))`
    /// returns an opaque value that owns that exact Tauri update and can install
    /// only through it.
    pub async fn check(&self) -> Result<Option<Update>> {
        self.check_with(|_| {}).await
    }

    /// Check once while reporting coarse, truthful progress phases.
    ///
    /// Phases carry no invented percentages. Download and reconstruction may
    /// repeat when a failed delta attempt safely falls back to another source.
    pub async fn check_with<F>(&self, progress: F) -> Result<Option<Update>>
    where
        F: Fn(ProgressEvent) + Send + Sync + 'static,
    {
        let progress: ProgressCallback = Arc::new(progress);
        progress(ProgressEvent::Checking);

        #[cfg(feature = "test-support")]
        let updater = if let Some(endpoint) = &self.config.endpoint_override {
            let endpoint = endpoint.parse().map_err(|e| {
                Error::Configuration(format!("parsing the test updater endpoint: {e}"))
            })?;
            self.app
                .updater_builder()
                .endpoints(vec![endpoint])
                .map_err(|e| Error::Updater(e.to_string()))?
                .build()
                .map_err(|e| Error::Updater(e.to_string()))?
        } else {
            self.app
                .updater()
                .map_err(|e| Error::Updater(e.to_string()))?
        };

        #[cfg(not(feature = "test-support"))]
        let updater = self
            .app
            .updater()
            .map_err(|e| Error::Updater(e.to_string()))?;

        let update = updater
            .check()
            .await
            .map_err(|e| Error::Updater(e.to_string()))?;

        let Some(update) = update else {
            progress(ProgressEvent::Finished);
            return Ok(None);
        };

        Ok(Some(Update {
            update,
            config: Arc::clone(&self.config),
            progress,
        }))
    }

    /// Check once and install when an update exists.
    ///
    /// This convenience method returns [`Outcome::UpToDate`] for `None`. It
    /// performs the same single official check as [`Self::check`].
    pub async fn check_and_install(&self) -> Result<Outcome> {
        self.check_and_install_with(|_| {}).await
    }

    /// Check and install while reporting coarse progress phases.
    pub async fn check_and_install_with<F>(&self, progress: F) -> Result<Outcome>
    where
        F: Fn(ProgressEvent) + Send + Sync + 'static,
    {
        match self.check_with(progress).await? {
            Some(update) => update.install().await,
            None => Ok(Outcome::UpToDate {
                diagnostics: self.config.diagnostics.clone(),
            }),
        }
    }

    /// Describe cache state for this repository's feature-gated E2E harness.
    #[cfg(feature = "test-support")]
    pub fn cache_state_for_tests(&self) -> String {
        let Some(cache) = &self.config.cache else {
            return "unavailable".to_owned();
        };
        match cache.state() {
            Ok(state) => {
                let describe = |entry: Option<&tauri_updater_delta_core::cache::CacheEntry>| {
                    entry
                        .map(|entry| format!("{}@{}", entry.version, entry.compressed_blake3))
                        .unwrap_or_else(|| "none".to_owned())
                };
                format!(
                    "active={} pending={} bytes={}",
                    describe(state.active.as_ref()),
                    describe(state.pending.as_ref()),
                    cache.blobs().size_on_disk().unwrap_or(0),
                )
            }
            Err(error) => format!("error {error}"),
        }
    }
}

/// An update returned by one authoritative Tauri updater check.
///
/// The constructor and fields are private. Internally this value owns the real
/// Tauri `Update`, derives the release identity from it, and installs through
/// that same object. Applications cannot provide an identity or installer and
/// therefore cannot pair values from different checks.
///
/// The old low-level session is intentionally absent from the public API:
///
/// ```compile_fail
/// use tauri_plugin_updater_delta::UpdateSession;
/// ```
///
/// The verified install handoff remains private as well:
///
/// ```compile_fail
/// use tauri_plugin_updater_delta::TauriInstall;
/// ```
pub struct Update {
    update: TauriUpdate,
    config: Arc<RuntimeConfig>,
    progress: ProgressCallback,
}

impl Update {
    /// Version this update will install, as Tauri's own check resolved it.
    pub fn version(&self) -> &str {
        &self.update.version
    }

    /// Version currently installed, as Tauri determined it.
    pub fn current_version(&self) -> &str {
        &self.update.current_version
    }

    /// Human-readable release notes, when the updater document supplied them.
    pub fn notes(&self) -> Option<&str> {
        self.update.body.as_deref()
    }

    /// Download, reconstruct when possible, verify, cache, and install.
    ///
    /// On the first update after adopting the plugin there is no verified cached
    /// base, so this normally returns a Full outcome and stages the official
    /// artifact. After the updated app launches, automatic reconciliation makes
    /// that artifact eligible for a later TarDelta. A missing or corrupt cache,
    /// an unavailable patch, or a reconstruction failure safely degrades to
    /// Full where policy permits.
    ///
    /// Authenticated identity contradictions, downgrade policy, and final
    /// signature failures never fall back. They return [`Error::Refused`] or
    /// [`Error::Signature`] and no bytes reach installation.
    pub async fn install(self) -> Result<Outcome> {
        let progress = Arc::clone(&self.progress);
        let result = tauri::async_runtime::spawn_blocking(move || self.install_blocking()).await;
        let outcome = result.map_err(|e| Error::Task(e.to_string()))??;
        progress(ProgressEvent::Finished);
        Ok(outcome)
    }

    fn install_blocking(self) -> Result<Outcome> {
        let identity = identity_of(&self.update);
        let mut headers = self.update.headers.clone();
        if !headers.contains_key(ACCEPT) {
            headers.insert(ACCEPT, HeaderValue::from_static("application/octet-stream"));
        }
        let mut transport = self
            .config
            .transport
            .clone()
            .headers_for_url(headers, identity.download_url())
            .no_proxy(self.update.no_proxy);
        if let Some(proxy) = &self.update.proxy {
            transport = transport.proxy(proxy.as_str());
        }
        let fetch = transport
            .build()
            .map_err(|e| Error::Fetch(format!("building the HTTP client: {e}")))?;

        let progress = |phase| {
            let event = match phase {
                FlowPhase::Downloading => ProgressEvent::Downloading,
                FlowPhase::Reconstructing => ProgressEvent::Reconstructing,
                FlowPhase::Verifying => ProgressEvent::Verifying,
                FlowPhase::Installing => ProgressEvent::Installing,
            };
            (self.progress)(event);
        };

        #[cfg(feature = "test-support")]
        let base = self.config.base_override.as_deref();
        #[cfg(not(feature = "test-support"))]
        let base = None;

        let report = run_update_detailed(
            &identity,
            &Context {
                pubkey: &self.config.pubkey,
                base,
                cache: self.config.cache.as_deref(),
                app_id: &self.config.app_id,
                work_dir: &self.config.work_dir,
                limits: self.config.limits,
            },
            &fetch,
            &TauriInstall::new(&self.update),
            &progress,
        )?;

        let mut diagnostics = self.config.diagnostics.clone();
        if let Some(message) = report.cache_write_error {
            let diagnostic = Diagnostic::CacheNotPersisted { message };
            log::warn!("{diagnostic}");
            diagnostics.push(diagnostic);
        }

        Ok(match report.outcome {
            FlowOutcome::InstalledFromTarDelta {
                downloaded,
                saved_against,
            } => Outcome::InstalledFromTarDelta {
                downloaded,
                full_download_size: saved_against,
                diagnostics,
            },
            FlowOutcome::InstalledFromDelta {
                downloaded,
                saved_against,
            } => Outcome::InstalledFromDirectDelta {
                downloaded,
                full_download_size: saved_against,
                diagnostics,
            },
            FlowOutcome::InstalledFromFullDownload => Outcome::InstalledFromFullDownload {
                downloaded: report.full_downloaded.unwrap_or(0),
                diagnostics,
            },
            FlowOutcome::UpToDate => Outcome::UpToDate { diagnostics },
        })
    }
}

fn identity_of(update: &TauriUpdate) -> UpdateIdentity {
    UpdateIdentity::new(
        &update.current_version,
        &update.version,
        &update.target,
        update.download_url.as_str(),
        &update.signature,
        update.raw_json.to_string(),
    )
}

/// Hands a verified artifact to the official updater's install step.
///
/// `Update::install` performs no signature verification in the pinned upstream
/// version. Keeping this type private and accepting only `VerifiedArtifact`
/// makes an unverified handoff unavailable through the shipping API.
struct TauriInstall<'a> {
    update: &'a TauriUpdate,
}

impl<'a> TauriInstall<'a> {
    fn new(update: &'a TauriUpdate) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_stay_inside_tauris_per_app_cache_directory() {
        let paths = ResolvedPaths::derive(Path::new("/safe/app-cache"), None, None);
        assert_eq!(
            paths.cache_dir,
            Path::new("/safe/app-cache/updater-delta/artifacts")
        );
        assert_eq!(
            paths.work_dir,
            Path::new("/safe/app-cache/updater-delta/transactions")
        );
    }

    #[test]
    fn path_overrides_are_independent_and_explicit() {
        let paths = ResolvedPaths::derive(
            Path::new("/safe/app-cache"),
            Some(PathBuf::from("/custom/cache")),
            Some(PathBuf::from("/custom/work")),
        );
        assert_eq!(paths.cache_dir, Path::new("/custom/cache"));
        assert_eq!(paths.work_dir, Path::new("/custom/work"));
    }

    #[test]
    fn a_cache_override_does_not_move_the_full_paths_workspace() {
        let paths = ResolvedPaths::derive(
            Path::new("/safe/app-cache"),
            Some(PathBuf::from("/broken/custom-cache")),
            None,
        );
        assert_eq!(paths.cache_dir, Path::new("/broken/custom-cache"));
        assert_eq!(
            paths.work_dir,
            Path::new("/safe/app-cache/updater-delta/transactions")
        );
    }
}
