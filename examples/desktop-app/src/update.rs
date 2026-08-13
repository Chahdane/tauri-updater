//! The real update path: Tauri checks, we substitute the download, Tauri installs.
//!
//! Not feature-gated — this is what the app does, and it is identical whether or
//! not the test control surface is compiled in. The control surface only
//! *triggers* it.
//!
//! # What this file is evidence of
//!
//! It is the smallest honest answer to "what does adopting this plugin cost a
//! Tauri developer?". Everything below the `updater.check()` call is the plugin;
//! everything above it is stock `tauri-plugin-updater` that the app would have
//! written anyway. See `docs/DECISIONS.md` #13 for why the update must originate
//! from Tauri's own check rather than a second fetch of our own.

use std::path::PathBuf;

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_updater_delta::flow::{run_update, Context, Outcome};
use tauri_plugin_updater_delta::{HttpFetch, Limits, TauriInstall, UpdateExt};
use tauri_updater_delta_core::cache::{
    ArtifactCache, CacheEntry, CacheLimits, Namespace, Reconciliation,
};
use tauri_updater_delta_core::manifest::{
    RECOMPRESSION_TAURI_APP_TAR_GZ_V1, REPRESENTATION_APP_TAR_GZ_V1,
};
use tauri_updater_delta_core::FileHash;

/// Where the manifest lives.
///
/// Read from the environment so the harness can serve on an ephemeral port;
/// a real app would hard-code this or take it from its own config.
///
/// Note this now feeds **only** Tauri's `endpoints()`. The plugin never fetches
/// it — that is the whole point of #13 — so there is exactly one place in this
/// app that knows a manifest URL exists.
pub fn manifest_url() -> String {
    std::env::var("DELTA_E2E_MANIFEST_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:0/manifest.json".to_owned())
}

/// The previously installed artifact, if one was supplied directly.
///
/// This is the **direct** patch path's base, and it stays an environment
/// variable because it is a harness affordance rather than something a shipping
/// app would do. The tar path's base comes from the real cache below, which is
/// the mechanism an app actually gets.
fn base_artifact() -> Option<PathBuf> {
    std::env::var("DELTA_E2E_BASE_ARTIFACT")
        .ok()
        .map(PathBuf::from)
}

/// Where the artifact cache lives.
///
/// A real app would use the platform's cache directory. The environment
/// override exists so the harness can point three separate app versions at one
/// cache and watch the state machine move.
pub fn cache_dir<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    if let Ok(dir) = std::env::var("DELTA_E2E_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    app.path()
        .app_cache_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("delta-updater")
}

/// Open the artifact cache for this installation.
///
/// The namespace binds everything that would make a cached artifact the wrong
/// base: which app, which platform and architecture, which signing key, and
/// which representation and recompression recipe this build implements. A
/// change to any of them empties the cache rather than reusing it — see
/// `tauri_updater_delta_core::cache`.
///
/// Note the identifiers come from **this build's** constants, not from a
/// manifest. They record what this client can do, which is what makes a cached
/// base usable; a manifest saying otherwise is a different question, asked at
/// selection time.
pub fn open_cache<R: Runtime>(app: &AppHandle<R>, pubkey: &str) -> Option<ArtifactCache> {
    let namespace = Namespace {
        bundle_id: app.config().identifier.clone(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        arch: std::env::consts::ARCH.to_owned(),
        pubkey_fingerprint: FileHash::of_bytes(pubkey.as_bytes()).to_hex(),
        representation: REPRESENTATION_APP_TAR_GZ_V1.to_owned(),
        recompression: RECOMPRESSION_TAURI_APP_TAR_GZ_V1.to_owned(),
    };
    match ArtifactCache::open(cache_dir(app), namespace, CacheLimits::default()) {
        Ok(cache) => Some(cache),
        Err(e) => {
            // A cache that will not open costs the tar path, not the update.
            eprintln!("delta: artifact cache unavailable: {e}");
            None
        }
    }
}

/// The public key this build verifies against, from `tauri.conf.json`.
///
/// Not passed in by the harness: a harness-supplied key would prove less.
pub fn pubkey<R: Runtime>(app: &AppHandle<R>) -> Result<String, String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| "no updater pubkey in tauri.conf.json".to_owned())
}

/// Bring the cache into line with the version that is actually running.
///
/// Called once at startup, before anything can plan an update. The version is
/// the one the running process reports about **itself** — `package_info()`,
/// compiled in — never a value read from a manifest or from the cache. That is
/// the whole basis for promotion: an artifact becomes the base for future
/// patches only when the application has demonstrably come back up running it.
pub fn reconcile_on_launch<R: Runtime>(app: &AppHandle<R>) -> String {
    let Ok(pubkey) = pubkey(app) else {
        return "no-pubkey".to_owned();
    };
    let Some(cache) = open_cache(app, &pubkey) else {
        return "no-cache".to_owned();
    };
    let running = app.package_info().version.to_string();
    match cache.reconcile(&running) {
        Ok(Reconciliation::Promoted { version }) => format!("promoted {version}"),
        Ok(Reconciliation::DiscardedPending { staged, running }) => {
            format!("discarded-pending staged={staged} running={running}")
        }
        Ok(Reconciliation::NothingStaged) => "nothing-staged".to_owned(),
        Ok(Reconciliation::NamespaceReset) => "namespace-reset".to_owned(),
        Err(e) => format!("error {e}"),
    }
}

/// A one-line description of the cache's current state, for the harness.
pub fn cache_state<R: Runtime>(app: &AppHandle<R>) -> String {
    let Ok(pubkey) = pubkey(app) else {
        return "no-pubkey".to_owned();
    };
    let Some(cache) = open_cache(app, &pubkey) else {
        return "no-cache".to_owned();
    };
    match cache.state() {
        Ok(state) => {
            let describe = |entry: Option<&CacheEntry>| match entry {
                Some(e) => format!("{}@{}", e.version, e.compressed_blake3),
                None => "none".to_owned(),
            };
            format!(
                "active={} pending={} bytes={}",
                describe(state.active.as_ref()),
                describe(state.pending.as_ref()),
                cache.blobs().size_on_disk().unwrap_or(0),
            )
        }
        Err(e) => format!("error {e}"),
    }
}

/// Build the HTTP client for this build's transport policy.
///
/// The default is HTTPS-only, and in a release build a plain-HTTP URL is
/// refused outright (`docs/DECISIONS.md` #19). The end-to-end harness serves
/// over loopback HTTP, and `cargo tauri build` is a release build, so the
/// harness needs the opt-in.
///
/// It is granted **only under `e2e-control`**, which is off by default and never
/// enabled by any default path. A normal build of this example cannot reach the
/// insecure branch — it is not a disabled flag, it is absent code. Same
/// discipline as the control surface itself: a test-only relaxation that can
/// ship is a vulnerability added deliberately and then forgotten.
fn http_client() -> Result<HttpFetch, String> {
    #[cfg(feature = "e2e-control")]
    {
        HttpFetch::builder()
            .dangerous_insecure_transport_protocol(true)
            .build()
            .map_err(|e| format!("http client: {e}"))
    }
    #[cfg(not(feature = "e2e-control"))]
    {
        HttpFetch::new().map_err(|e| format!("http client: {e}"))
    }
}

/// Run one update, returning a short description of what happened.
pub fn run<R: Runtime>(app: &AppHandle<R>) -> Result<String, String> {
    // The public key comes from tauri.conf.json — the same value the official
    // updater would verify against.
    let pubkey = pubkey(app)?;

    // Tauri's own check. This is the step that proves the superset manifest
    // works: the official updater parses it with no knowledge of deltas, and
    // keeps the whole document on `Update::raw_json` for us to read.
    let updater = app
        .updater_builder()
        .endpoints(vec![manifest_url()
            .parse()
            .map_err(|e| format!("bad manifest url: {e}"))?])
        .map_err(|e| format!("configuring the updater: {e}"))?
        .build()
        .map_err(|e| format!("building the updater: {e}"))?;

    let update = tauri::async_runtime::block_on(updater.check())
        .map_err(|e| format!("update check failed: {e}"))?;

    let Some(update) = update else {
        return Ok("up-to-date".to_owned());
    };

    // Out of the async context now, so the blocking flow is safe to run.
    let work_dir = std::env::temp_dir().join("delta-updater-example-work");
    let fetch = http_client()?;
    let base = base_artifact();
    let cache = open_cache(app, &pubkey);

    let outcome = run_update(
        // One line, and it is the entire trust argument: the release we install
        // is the release Tauri checked, because it is the same object.
        &update.delta_identity(),
        &Context {
            pubkey: &pubkey,
            base: base.as_deref(),
            cache: cache.as_ref(),
            work_dir: &work_dir,
            limits: Limits::default(),
        },
        &fetch,
        // The real seam. Everything before this produced a VerifiedArtifact;
        // this hands it to the official updater's own install step.
        &TauriInstall::new(&update),
    )
    .map_err(|e| format!("update failed: {e}"))?;

    Ok(match outcome {
        // Distinct strings, because the harness asserts on which path ran. A
        // final hash cannot tell a tar delta from a full download.
        Outcome::InstalledFromTarDelta {
            downloaded,
            saved_against,
        } => format!("installed-from-tar-delta downloaded={downloaded} full={saved_against}"),
        Outcome::InstalledFromDelta {
            downloaded,
            saved_against,
        } => format!("installed-from-delta downloaded={downloaded} full={saved_against}"),
        Outcome::InstalledFromFullDownload => "installed-from-full-download".to_owned(),
        Outcome::UpToDate => "up-to-date".to_owned(),
    })
}
