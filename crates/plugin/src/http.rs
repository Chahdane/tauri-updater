//! The network boundary: a blocking HTTP implementation of [`Fetch`].
//!
//! Blocking rather than async on purpose. The work either side of a download —
//! hashing hundreds of megabytes, decompressing a patch — is CPU-bound and
//! blocking anyway, so the whole flow belongs on a blocking task. Callers run it
//! under `tauri::async_runtime::spawn_blocking`; using `reqwest::blocking`
//! inside that is fine, and keeps [`Fetch`] a one-method trait that tests can
//! implement with a `HashMap`.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use tauri_updater_delta_core::client::Fetch;

/// Downloads over HTTPS, streaming to disk.
pub struct HttpFetch {
    client: reqwest::blocking::Client,
}

impl HttpFetch {
    /// Build a fetcher with sensible timeouts.
    pub fn new() -> std::result::Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "tauri-plugin-updater-delta/",
                env!("CARGO_PKG_VERSION")
            ))
            .connect_timeout(Duration::from_secs(30))
            // No overall timeout: a patch is small but an artifact may not be,
            // and a slow connection is not a failure.
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }
}

impl Fetch for HttpFetch {
    fn fetch(&self, url: &str, out: &Path) -> std::result::Result<(), String> {
        let mut response = self
            .client
            .get(url)
            .send()
            .map_err(|e| format!("request failed: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("server returned {}", response.status()));
        }

        let mut file = std::fs::File::create(out).map_err(|e| format!("creating {out:?}: {e}"))?;
        // Streamed, so a large artifact is never fully resident here. It becomes
        // resident only once, inside the verification token.
        std::io::copy(&mut response, &mut file).map_err(|e| format!("writing {out:?}: {e}"))?;
        file.flush().map_err(|e| format!("flushing {out:?}: {e}"))?;
        Ok(())
    }
}
