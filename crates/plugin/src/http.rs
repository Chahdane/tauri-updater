//! The network boundary: a blocking HTTP implementation of [`Fetch`], with an
//! explicit policy about what it will and will not do.
//!
//! Blocking rather than async on purpose. The work either side of a download —
//! hashing hundreds of megabytes, decompressing a patch — is CPU-bound and
//! blocking anyway, so the whole flow belongs on a blocking task. Callers run it
//! under `tauri::async_runtime::spawn_blocking`; using `reqwest::blocking`
//! inside that is fine, and keeps [`Fetch`] a one-method trait that tests can
//! implement with a `HashMap`.
//!
//! # Everything here is policy, not plumbing
//!
//! A download is the one place an attacker gets to choose how much work we do.
//! The URL, the redirect chain, the `Content-Length`, the body length and the
//! response rate are all theirs. So each of those is bounded here rather than
//! trusted, and the bounds live on this side of the [`Fetch`] trait so that the
//! trait stays one method wide and tests keep faking it with a map.
//!
//! | What the server controls | What bounds it |
//! | --- | --- |
//! | URL scheme | [`https` required](HttpFetchBuilder::dangerous_insecure_transport_protocol) |
//! | Redirect chain | [`max_redirects`](HttpFetchBuilder::max_redirects), no HTTPS→HTTP |
//! | Time to first byte | [`connect_timeout`](HttpFetchBuilder::connect_timeout) |
//! | Time to last byte | [`request_timeout`](HttpFetchBuilder::request_timeout) |
//! | Declared length | rejected before the body is read |
//! | Actual length | counted while streaming |
//! | A failed transfer | written to a `.part` file, never the real name |

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use tauri_updater_delta_core::client::Fetch;

/// Largest response this will accept by default, in bytes.
///
/// Sized for a desktop installer with room to spare, not for a number that
/// sounds generous. A response beyond this is a resource-exhaustion attempt or a
/// misconfiguration, and both should stop here rather than fill a disk.
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Default ceiling on a whole request, headers to last byte.
///
/// Generous because a large artifact on a slow connection is ordinary, not a
/// failure. It exists to bound the pathological case — a server that accepts the
/// connection and then stalls forever — which no other timeout catches.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Default ceiling on establishing the connection.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default redirect budget.
///
/// Enough for the hop patterns real hosting uses — GitHub Releases answers with
/// a 302 to `objects.githubusercontent.com` — and far short of a loop.
pub const DEFAULT_MAX_REDIRECTS: usize = 5;

/// Downloads over HTTPS, streaming to disk, with every server-controlled
/// quantity bounded.
pub struct HttpFetch {
    client: reqwest::blocking::Client,
    insecure: bool,
    max_response_bytes: u64,
}

/// Configures an [`HttpFetch`].
pub struct HttpFetchBuilder {
    insecure: bool,
    max_response_bytes: u64,
    connect_timeout: Duration,
    request_timeout: Duration,
    max_redirects: usize,
}

impl Default for HttpFetchBuilder {
    fn default() -> Self {
        Self {
            insecure: false,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_redirects: DEFAULT_MAX_REDIRECTS,
        }
    }
}

impl HttpFetchBuilder {
    /// Allow non-HTTPS URLs.
    ///
    /// **Named to match `tauri-plugin-updater`'s own
    /// `dangerousInsecureTransportProtocol` deliberately.** Upstream applies the
    /// same rule to updater endpoints (`config.rs:145`), and a developer who has
    /// already opted in there should not then meet a second, differently-named
    /// refusal from this plugin. Where our policy and Tauri's answer the same
    /// question they must not be able to disagree — the same reasoning that
    /// makes us match Tauri's version comparator in `docs/DECISIONS.md` #14.
    ///
    /// This does **not** re-enable an HTTPS→HTTP redirect. Starting on plain
    /// HTTP is a development choice; being moved off HTTPS mid-chain is someone
    /// else's choice, and that stays refused.
    pub fn dangerous_insecure_transport_protocol(mut self, allow: bool) -> Self {
        self.insecure = allow;
        self
    }

    /// Largest response body to accept, in bytes.
    pub fn max_response_bytes(mut self, bytes: u64) -> Self {
        self.max_response_bytes = bytes;
        self
    }

    /// Ceiling on establishing the connection.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Ceiling on the whole request, headers to last byte.
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// How many redirects to follow before giving up.
    pub fn max_redirects(mut self, max: usize) -> Self {
        self.max_redirects = max;
        self
    }

    /// Build the fetcher.
    pub fn build(self) -> std::result::Result<HttpFetch, String> {
        let insecure = self.insecure;
        let max_redirects = self.max_redirects;

        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= max_redirects {
                return attempt.error(format!("more than {max_redirects} redirects"));
            }

            // A downgrade is refused even when plain HTTP is otherwise allowed.
            // Starting on HTTP is a choice the app made; being moved off HTTPS
            // part-way through is a choice the server made.
            let came_from_https = attempt
                .previous()
                .last()
                .is_some_and(|url| url.scheme() == "https");
            let scheme = attempt.url().scheme().to_owned();
            if came_from_https && scheme != "https" {
                return attempt.error(format!("refusing an HTTPS to {scheme} redirect"));
            }

            let target = attempt.url().as_str().to_owned();
            if let Err(reason) = scheme_allowed(&target, insecure) {
                return attempt.error(reason);
            }

            attempt.follow()
        });

        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "tauri-plugin-updater-delta/",
                env!("CARGO_PKG_VERSION")
            ))
            .connect_timeout(self.connect_timeout)
            // Bounds the whole request, which is the only thing that catches a
            // server accepting the connection and then never sending a byte.
            .timeout(self.request_timeout)
            .redirect(policy)
            .build()
            .map_err(|e| e.to_string())?;

        Ok(HttpFetch {
            client,
            insecure,
            max_response_bytes: self.max_response_bytes,
        })
    }
}

impl HttpFetch {
    /// Build a fetcher with the default policy: HTTPS only, bounded everything.
    pub fn new() -> std::result::Result<Self, String> {
        HttpFetchBuilder::default().build()
    }

    /// Start configuring a fetcher.
    pub fn builder() -> HttpFetchBuilder {
        HttpFetchBuilder::default()
    }
}

/// Whether an I/O error from the body reader is really a deadline.
///
/// Fiddlier than it looks. reqwest reports a request-deadline hit during a body
/// read as `error::decode(TimedOut).into_io()` — an `io::Error` of kind `Other`
/// wrapping a `reqwest::Error` whose *source* is the timeout. And
/// `io::Error::source()` returns the inner error's source rather than the inner
/// error, so walking the source chain steps straight over the `reqwest::Error`
/// and never sees it. `get_ref()` is the only way to reach it.
fn is_timeout(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::TimedOut {
        return true;
    }
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
        .is_some_and(|e| e.is_timeout())
}

/// Apply the transport policy to one URL.
///
/// Mirrors `tauri-plugin-updater`'s `validate_endpoints` step for step: allowed
/// outright when the caller opted in, warned-but-allowed in a development build,
/// and refused in a release build. Matching it means a project configured for
/// one is configured for both.
fn scheme_allowed(url: &str, insecure: bool) -> std::result::Result<(), String> {
    if insecure {
        return Ok(());
    }

    let scheme = url.split("://").next().unwrap_or_default();
    if scheme.eq_ignore_ascii_case("https") {
        return Ok(());
    }

    #[cfg(debug_assertions)]
    {
        eprintln!(
            "[WARNING] the update URL {url:?} does not use https. This is allowed \
             in development but will fail in release builds."
        );
        eprintln!("[WARNING] if that is intended, enable dangerous_insecure_transport_protocol");
        Ok(())
    }

    #[cfg(not(debug_assertions))]
    Err(format!(
        "refusing to fetch {url:?} over an insecure transport: updates must use \
         https. If that is intended, enable dangerous_insecure_transport_protocol."
    ))
}

impl Fetch for HttpFetch {
    fn fetch(&self, url: &str, out: &Path) -> std::result::Result<(), String> {
        scheme_allowed(url, self.insecure)?;

        let mut response = self
            .client
            .get(url)
            .send()
            .map_err(|e| format!("request failed: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("server returned {}", response.status()));
        }

        // Cheapest possible rejection: if the server admits up front that the
        // body is oversized, stop before reading any of it. Never *trusted* —
        // the streaming counter below is what actually enforces the bound.
        if let Some(declared) = response.content_length() {
            if declared > self.max_response_bytes {
                return Err(format!(
                    "response declares {declared} bytes, over the {} byte limit",
                    self.max_response_bytes
                ));
            }
        }

        // Written beside the destination and renamed only on success, so a
        // failed or truncated transfer can never leave something at a path that
        // later looks like a finished download.
        let partial = out.with_extension("part");
        let result = self.stream_to(&mut response, &partial);

        match result {
            Ok(()) => std::fs::rename(&partial, out)
                .map_err(|e| format!("promoting {partial:?} to {out:?}: {e}")),
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
                Err(e)
            }
        }
    }
}

impl HttpFetch {
    /// Stream the body to `partial`, stopping if it exceeds the cap.
    ///
    /// The count is of bytes actually received, so a lying or absent
    /// `Content-Length` and a chunked body are all bounded by the same check.
    fn stream_to(
        &self,
        response: &mut reqwest::blocking::Response,
        partial: &Path,
    ) -> std::result::Result<(), String> {
        let mut file =
            std::fs::File::create(partial).map_err(|e| format!("creating {partial:?}: {e}"))?;

        let mut buffer = vec![0u8; 64 * 1024];
        let mut written: u64 = 0;

        loop {
            let read = response.read(&mut buffer).map_err(|e| {
                // reqwest surfaces a request-deadline hit during the body read
                // as a decode failure whose source is a timeout, so the plain
                // message would not mention time at all. Say so explicitly:
                // "error decoding response body" sends someone hunting for a
                // corrupt artifact when the server simply stopped talking.
                if is_timeout(&e) {
                    format!("timed out reading the response body after the request deadline: {e}")
                } else {
                    format!("reading the response body: {e}")
                }
            })?;
            if read == 0 {
                break;
            }

            written += read as u64;
            if written > self.max_response_bytes {
                return Err(format!(
                    "response body exceeded the {} byte limit",
                    self.max_response_bytes
                ));
            }

            file.write_all(&buffer[..read])
                .map_err(|e| format!("writing {partial:?}: {e}"))?;
        }

        file.flush()
            .map_err(|e| format!("flushing {partial:?}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_is_always_allowed() {
        assert!(scheme_allowed("https://example.com/a", false).is_ok());
        assert!(scheme_allowed("HTTPS://example.com/a", false).is_ok());
    }

    #[test]
    fn the_opt_in_allows_plain_http() {
        assert!(scheme_allowed("http://127.0.0.1:8080/manifest.json", true).is_ok());
    }

    #[test]
    fn plain_http_follows_the_build_profile() {
        let verdict = scheme_allowed("http://example.com/a", false);

        // Matching tauri-plugin-updater exactly: a development build warns and
        // continues, a release build refuses. Asserting both arms here means the
        // test states the policy rather than the profile it happened to run in.
        if cfg!(debug_assertions) {
            assert!(
                verdict.is_ok(),
                "development builds allow http with a warning, as upstream does"
            );
        } else {
            let reason = verdict.expect_err("release builds must refuse http");
            assert!(
                reason.contains("dangerous_insecure_transport_protocol"),
                "the refusal must name the opt-in or a developer is stuck: {reason}"
            );
        }
    }

    #[test]
    fn a_builder_with_no_changes_is_the_default_policy() {
        let fetch = HttpFetch::new().expect("build");
        assert!(!fetch.insecure);
        assert_eq!(fetch.max_response_bytes, DEFAULT_MAX_RESPONSE_BYTES);
    }

    #[test]
    fn limits_are_configurable() {
        let fetch = HttpFetch::builder()
            .max_response_bytes(1024)
            .dangerous_insecure_transport_protocol(true)
            .build()
            .expect("build");
        assert_eq!(fetch.max_response_bytes, 1024);
        assert!(fetch.insecure);
    }
}
