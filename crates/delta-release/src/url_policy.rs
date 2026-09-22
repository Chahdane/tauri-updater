//! One URL policy, applied by both the generator and the checker.
//!
//! # Why this module exists
//!
//! There were two policies. [`build_release`](crate::build_release) narrowed its
//! `allow_insecure_urls` opt-in to loopback, exactly as its documentation said.
//! [`verify_release`](crate::verify::verify_release) — the independent gate that
//! reads the manifest back as a stranger — accepted **every** `http://` URL
//! whenever its flag was set, while its CLI help said the flag was "for loopback
//! rehearsals only".
//!
//! That never weakened the production workflow, which passes the flag nowhere.
//! It disagreed in precisely the mode the flag exists for: a loopback rehearsal
//! could generate a manifest the generator would have refused, and the gate that
//! exists to catch such a manifest would wave it through. A checker that is more
//! permissive than the generator cannot catch a generator bug, which is the only
//! reason to have a separate checker at all.
//!
//! So there is now one function, and both call it.
//!
//! # Why a URL parser rather than string handling
//!
//! The generator's own loopback check was `rest.split(['/', ':']).next()`,
//! matched against `"127.0.0.1" | "localhost" | "[::1]"`. Splitting on `:`
//! truncates an IPv6 literal at its first colon, so `http://[::1]:8080/x`
//! produced the host `"["` and was refused — while the *documentation* of the
//! flag, and the `"[::1]"` arm of that very match, both promised it worked. A
//! check whose allow-list names a spelling it can never produce is a check
//! nobody has run.
//!
//! Deciding where a URL points is not a delimiter problem. A host may carry
//! userinfo, a port, brackets, percent-encoding or a trailing dot, and each of
//! those is another way for a hand-rolled split to disagree with the resolver
//! that will actually be used.
//!
//! [`url`] does the parsing that RFC 3986 actually describes, and
//! [`std::net`]'s own `is_loopback` decides what loopback means, so neither
//! question is answered by a list of spellings.

use url::{Host, Url};

/// Whether a plain-HTTP URL may appear in a release at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpPolicy {
    /// HTTPS only. The policy every public release is published under.
    HttpsOnly,
    /// HTTPS, or plain HTTP to a loopback address.
    ///
    /// For this repository's end-to-end harness, which serves from `127.0.0.1`,
    /// and for nothing else. Deliberately not a general escape hatch: a general
    /// escape hatch is how the safe default gets switched off once and left off.
    LoopbackHttpAllowed,
}

impl HttpPolicy {
    /// The policy an `allow_insecure_urls` flag selects.
    pub fn from_insecure_flag(allow_insecure: bool) -> Self {
        if allow_insecure {
            Self::LoopbackHttpAllowed
        } else {
            Self::HttpsOnly
        }
    }
}

/// Check one URL against `policy`, returning why it is unpublishable.
///
/// `what` names the URL for the message — "the installer URL", "the 1.0.0 patch
/// URL" — because a release carries several and "this URL is plain HTTP" does
/// not say which one to fix.
pub fn check_url(what: &str, url: &str, policy: HttpPolicy) -> Result<(), String> {
    let parsed =
        Url::parse(url).map_err(|e| format!("{what} is not an http(s) URL ({url}): {e}"))?;

    match parsed.scheme() {
        "https" => Ok(()),
        "http" => match policy {
            HttpPolicy::HttpsOnly => Err(format!(
                "{what} is plain HTTP ({url}). Production clients refuse a non-HTTPS \
                 artifact URL, so this release would be rejected by every client that \
                 fetched it. Use https, or enable the loopback-only insecure opt-in if \
                 this is the local end-to-end harness."
            )),
            HttpPolicy::LoopbackHttpAllowed if is_loopback(&parsed) => Ok(()),
            HttpPolicy::LoopbackHttpAllowed => Err(format!(
                "{what} is plain HTTP to {:?}. The insecure opt-in covers loopback \
                 only; a release served from anywhere else must use https.",
                parsed.host_str().unwrap_or("")
            )),
        },
        other => Err(format!(
            "{what} is not an http(s) URL ({url}): scheme is {other:?}"
        )),
    }
}

/// Whether `url`'s host is the local machine, by the address rather than by its
/// spelling.
///
/// `localhost` is accepted by name because it is the one host name whose
/// resolution is fixed by RFC 6761. Every other name is a name, and a name is
/// resolved by a resolver an attacker may control, so no other name can be
/// loopback for this purpose regardless of what it resolves to today.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(addr)) => addr.is_loopback(),
        Some(Host::Ipv6(addr)) => addr.is_loopback(),
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRICT: HttpPolicy = HttpPolicy::HttpsOnly;
    const LOOPBACK: HttpPolicy = HttpPolicy::LoopbackHttpAllowed;

    #[test]
    fn https_is_always_publishable() {
        check_url("the installer URL", "https://example.com/a.tar.gz", STRICT)
            .expect("https under the strict policy");
        check_url(
            "the installer URL",
            "https://example.com/a.tar.gz",
            LOOPBACK,
        )
        .expect("https under the loopback policy");
    }

    #[test]
    fn plain_http_is_refused_without_the_opt_in() {
        let reason = check_url("the installer URL", "http://example.com/a", STRICT)
            .expect_err("plain http must be refused");
        assert!(reason.contains("plain HTTP"), "{reason}");
    }

    #[test]
    fn the_opt_in_reaches_loopback_and_stops_there() {
        for url in [
            "http://127.0.0.1:8080/manifest.json",
            "http://127.0.0.1/manifest.json",
            "http://localhost:8080/manifest.json",
            "http://LOCALHOST:8080/manifest.json",
            // 127.0.0.0/8 in full, not just the one address people write.
            "http://127.1.2.3:8080/manifest.json",
        ] {
            check_url("the patch URL", url, LOOPBACK)
                .unwrap_or_else(|e| panic!("{url} should be permitted by the opt-in: {e}"));
        }

        for url in [
            "http://example.com/a",
            "http://10.0.0.1/a",
            // A name that merely starts with a loopback address is a name.
            "http://127.0.0.1.evil.example/a",
            "http://[2001:db8::1]:8080/a",
            // Userinfo is not the host, whatever it is dressed as.
            "http://127.0.0.1@evil.example/a",
        ] {
            let reason = check_url("the patch URL", url, LOOPBACK)
                .expect_err(&format!("{url} must not be permitted by the opt-in"));
            assert!(
                reason.contains("loopback only"),
                "{url} should be refused for not being loopback, got: {reason}"
            );
        }
    }

    /// The bug the parser fixes, stated as a test.
    ///
    /// `[::1]` was in the generator's own allow-list and in the flag's
    /// documentation, and the hand-rolled host split truncated it at the first
    /// colon, so the one documented IPv6 loopback spelling never worked.
    #[test]
    fn the_documented_ipv6_loopback_spelling_works() {
        check_url(
            "the installer URL",
            "http://[::1]:8080/manifest.json",
            LOOPBACK,
        )
        .expect("[::1] is loopback and is named in the flag's documentation");
        check_url("the installer URL", "http://[::1]/manifest.json", LOOPBACK)
            .expect("[::1] without a port is loopback too");
    }

    #[test]
    fn a_non_http_scheme_is_refused_under_both_policies() {
        for policy in [STRICT, LOOPBACK] {
            let reason = check_url("the installer URL", "file:///etc/passwd", policy)
                .expect_err("a file URL is not publishable");
            assert!(reason.contains("not an http(s) URL"), "{reason}");
        }
    }

    #[test]
    fn something_that_is_not_a_url_is_refused() {
        let reason = check_url("the installer URL", "dist/app.tar.gz", STRICT)
            .expect_err("a bare path is not a URL");
        // Worded like every other scheme refusal on purpose: a caller reading a
        // CI log should not have to know whether the parse or the scheme check
        // rejected it, only that the field is not a publishable URL.
        assert!(reason.contains("is not an http(s) URL"), "{reason}");
    }
}
