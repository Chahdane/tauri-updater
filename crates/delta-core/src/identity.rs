//! One release, described once: the identity an update is bound to.
//!
//! # The problem this exists to solve
//!
//! An earlier design fetched the release manifest twice — once by Tauri's
//! `Updater::check()`, and again by this crate to plan the delta. Those are two
//! independently obtained descriptions of "what the update is", and nothing
//! compared them. A server could answer the first request with a new version so
//! Tauri's semver gate passed, and the second with an *older, genuinely signed*
//! release. Signature verification would succeed — an old release's signature is
//! valid forever — and the user would be silently downgraded, with no signing
//! key involved at any point.
//!
//! The distinction that makes this possible is worth stating plainly, because
//! the old code did not:
//!
//! > **Artifact authenticity is not manifest authenticity.**
//! >
//! > A valid minisign signature proves the *bytes* were signed by the expected
//! > key. It proves nothing about the version, the URL, the sizes, the digests,
//! > or any other manifest field, because none of those are covered by it. The
//! > manifest is **not** authenticated, and this module does not make it so.
//!
//! # What this module does instead
//!
//! It does not authenticate the manifest. It removes the *second description*.
//!
//! Tauri already retains the exact JSON document it fetched, verbatim, on
//! `Update::raw_json`, along with the target, URL and signature it selected from
//! it. [`UpdateIdentity`] carries those forward as the single authority, and the
//! delta layer is read out of that same document rather than fetched again.
//! With one document there is no second answer to disagree with, so
//!
//! ```text
//! checked_target == delta_target == verified_target == installed_target
//! ```
//!
//! holds *structurally* for the first three — they are views of one parse — and
//! the fourth is closed by [`VerifiedArtifact`](crate::VerifiedArtifact) owning
//! the bytes it authenticated.
//!
//! What remains are the seams where one document can still be read two ways:
//! our parse diverging from Tauri's, and the platform entry we look up
//! diverging from the one Tauri selected. Both are checked here, and both
//! [`Refusal`] rather than fall back.
//!
//! # Why a refusal is not a fallback
//!
//! Every other delta failure resolves to a full download, because every other
//! failure is a statement about one transfer. A version-policy or identity
//! violation is a statement about *which release we are being pointed at*, and
//! the full-download path is pointed at a release by the same document. Falling
//! back would re-run the attack down the other branch while presenting itself as
//! the safe option.

use std::fmt;

use semver::Version;

/// The single authoritative description of the release being installed.
///
/// Built from the `Update` that Tauri's own check returned — see the plugin
/// crate's `UpdateExt::delta_identity`. Deliberately Tauri-independent so the
/// whole policy below is testable without an app, a runtime, or a network.
///
/// Every field originates from **one** HTTP response. That is the entire point:
/// there is no second fetch whose answer could differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateIdentity {
    current_version: String,
    version: String,
    target: String,
    download_url: String,
    signature: String,
    raw_json: String,
}

impl UpdateIdentity {
    /// Assemble an identity from the fields of a checked update.
    ///
    /// The plugin builds this from `tauri_plugin_updater::Update`; tests build
    /// it directly. All six values must come from the same update check, or the
    /// guarantee this type exists to provide does not hold — which is why the
    /// only supported construction in the shipping path is
    /// `UpdateExt::delta_identity`, not this constructor.
    pub fn new(
        current_version: impl Into<String>,
        version: impl Into<String>,
        target: impl Into<String>,
        download_url: impl Into<String>,
        signature: impl Into<String>,
        raw_json: impl Into<String>,
    ) -> Self {
        Self {
            current_version: current_version.into(),
            version: version.into(),
            target: target.into(),
            download_url: download_url.into(),
            signature: signature.into(),
            raw_json: raw_json.into(),
        }
    }

    /// Version currently installed, as Tauri determined it.
    pub fn current_version(&self) -> &str {
        &self.current_version
    }

    /// Version Tauri's check resolved to — the `checked_target`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Tauri's `Update.target` field, verbatim.
    ///
    /// **This is not a manifest platform key, and must never be used as one.**
    /// It is `updater_os()` — bare `"darwin"`, `"linux"`, `"windows"` — unless
    /// the app explicitly configured a target override (`updater.rs:403`).
    ///
    /// The confusion is upstream's naming: the *field* `Update.target` holds the
    /// OS, while the free function `updater::target()` returns `{os}-{arch}`.
    /// Upstream's own endpoint templates treat them as separate variables —
    /// `{{target}}/{{arch}}` expands to `/darwin/aarch64/`.
    ///
    /// An earlier design used this as the delta platform key. It silently
    /// matched nothing, so every update fell back to a full download while
    /// looking healthy. See `docs/DECISIONS.md` #22; the platform entry is now
    /// resolved from [`download_url`](Self::download_url) and
    /// [`signature`](Self::signature) instead.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The full-download URL **Tauri selected**.
    ///
    /// Authoritative. This crate deliberately does not reproduce Tauri's
    /// `{os}-{arch}-{installer}` then `{os}-{arch}` search order; it consumes
    /// the result of that search instead, so the two cannot diverge.
    pub fn download_url(&self) -> &str {
        &self.download_url
    }

    /// The signature **Tauri selected**, over the artifact at
    /// [`download_url`](Self::download_url).
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// The release document Tauri fetched, verbatim.
    ///
    /// The delta layer is parsed out of this rather than fetched separately.
    pub fn raw_json(&self) -> &str {
        &self.raw_json
    }
}

/// Why an update was refused outright rather than downgraded to a full download.
///
/// Each variant means the release we are being pointed at is not one we are
/// willing to install — never that a transfer failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The manifest disagrees with the update Tauri checked.
    ///
    /// One document read two ways. Either our parse diverged from Tauri's, or
    /// the delta layer describes a different artifact than the one Tauri
    /// selected for this platform.
    IdentityMismatch {
        /// Which field disagreed, e.g. `"version"` or `"signature"`.
        field: &'static str,
        /// What the checked update says.
        checked: String,
        /// What the delta metadata says.
        delta: String,
    },

    /// The target release is older than what is installed.
    ///
    /// The replay case: an attacker who can rewrite the manifest can point a
    /// client at a genuinely signed but vulnerable old release. Signatures do
    /// not expire, so verification cannot catch this — only version policy can.
    Downgrade {
        /// Version currently installed.
        current: String,
        /// Older version the manifest points at.
        target: String,
    },

    /// The manifest offers more than one platform entry for the artifact Tauri
    /// selected, and they disagree about what to install.
    ///
    /// Duplicate keys describing the *same* artifact are fine and are allowed.
    /// This is the case where they describe different ones, which means there is
    /// no single answer to "what is the target", so there is nothing safe to
    /// pick. Fails closed rather than choosing.
    AmbiguousPlatform {
        /// How many platform entries matched Tauri's selection.
        matches: usize,
    },

    /// A version relevant to that decision could not be parsed as semver.
    ///
    /// Refused rather than fallen back on: an unorderable version means the
    /// downgrade policy cannot be applied at all, and proceeding without it is
    /// exactly the hole the policy closes.
    UncomparableVersion {
        /// Which version failed to parse, e.g. `"installed"` or `"target"`.
        which: &'static str,
        /// The value that could not be parsed.
        value: String,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityMismatch {
                field,
                checked,
                delta,
            } => write!(
                f,
                "the delta metadata does not describe the update Tauri checked: \
                 {field} is {checked:?} in the checked update but {delta:?} in the delta layer"
            ),
            Self::Downgrade { current, target } => write!(
                f,
                "refusing to install {target}: older than the installed {current}"
            ),
            Self::AmbiguousPlatform { matches } => write!(
                f,
                "{matches} platform entries match the artifact Tauri selected and they \
                 disagree about the target; refusing rather than choosing one"
            ),
            Self::UncomparableVersion { which, value } => write!(
                f,
                "the {which} version {value:?} is not valid semver, so the downgrade \
                 policy cannot be applied"
            ),
        }
    }
}

/// The outcome of comparing the installed version against the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionVerdict {
    /// Target is newer. Proceed.
    Upgrade,
    /// Target is what is already installed. Nothing to do — not an error.
    UpToDate,
    /// Target is older, or unorderable. Refuse.
    Refused(Refusal),
}

/// Parse a version the way `tauri-plugin-updater` does.
///
/// Upstream's `parse_version` strips a leading `v` before handing the string to
/// semver (`updater.rs:1443`). Matching that matters: a manifest carrying
/// `"v1.2.0"` parses for Tauri, so if it did not parse here we would refuse a
/// release the official updater considers perfectly ordinary.
fn parse_version(value: &str) -> Option<Version> {
    Version::parse(value.trim_start_matches('v')).ok()
}

/// Apply the version policy to an installed/target pair.
///
/// Ordering is full semver, so prereleases sort below their release
/// (`1.0.0-beta < 1.0.0`) and build metadata is ignored for precedence, exactly
/// as the specification requires and as Tauri's own `>` comparison behaves.
pub fn evaluate_version(installed: &str, target: &str) -> VersionVerdict {
    let Some(installed_v) = parse_version(installed) else {
        return VersionVerdict::Refused(Refusal::UncomparableVersion {
            which: "installed",
            value: installed.to_owned(),
        });
    };
    let Some(target_v) = parse_version(target) else {
        return VersionVerdict::Refused(Refusal::UncomparableVersion {
            which: "target",
            value: target.to_owned(),
        });
    };

    match target_v.cmp(&installed_v) {
        std::cmp::Ordering::Greater => VersionVerdict::Upgrade,
        std::cmp::Ordering::Equal => VersionVerdict::UpToDate,
        std::cmp::Ordering::Less => VersionVerdict::Refused(Refusal::Downgrade {
            current: installed.to_owned(),
            target: target.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upgrade(from: &str, to: &str) {
        assert_eq!(
            evaluate_version(from, to),
            VersionVerdict::Upgrade,
            "{from} -> {to} should be an upgrade"
        );
    }

    fn refuses(from: &str, to: &str) -> Refusal {
        match evaluate_version(from, to) {
            VersionVerdict::Refused(reason) => reason,
            other => panic!("{from} -> {to} should be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_newer_target_is_an_upgrade() {
        upgrade("1.0.0", "1.0.1");
        upgrade("1.0.0", "1.1.0");
        upgrade("1.0.0", "2.0.0");
    }

    #[test]
    fn several_versions_behind_is_still_an_upgrade() {
        // Schema 1 patches from any previous version straight to the latest, so
        // being far behind is ordinary, not a special case.
        upgrade("1.0.0", "1.9.4");
        upgrade("0.1.0", "3.0.0");
    }

    #[test]
    fn the_same_version_is_up_to_date_not_an_error() {
        assert_eq!(evaluate_version("1.0.0", "1.0.0"), VersionVerdict::UpToDate);
    }

    #[test]
    fn an_older_target_is_refused() {
        // The replay case. The old artifact's signature is genuine and always
        // will be, so nothing downstream of here can catch this.
        let reason = refuses("2.0.0", "1.0.0");
        assert!(
            matches!(reason, Refusal::Downgrade { .. }),
            "expected a downgrade refusal, got {reason:?}"
        );
        refuses("1.0.1", "1.0.0");
        refuses("1.1.0", "1.0.9");
    }

    #[test]
    fn prereleases_order_by_semver() {
        // A prerelease sorts below its release...
        upgrade("1.0.0-beta", "1.0.0");
        upgrade("1.0.0-alpha", "1.0.0-beta");
        upgrade("1.0.0-beta.1", "1.0.0-beta.2");
        // ...so going back to one is a downgrade, not an upgrade.
        assert!(matches!(
            refuses("1.0.0", "1.0.0-beta"),
            Refusal::Downgrade { .. }
        ));
        assert!(matches!(
            refuses("1.0.0-beta.2", "1.0.0-beta.1"),
            Refusal::Downgrade { .. }
        ));
    }

    #[test]
    fn build_metadata_orders_as_the_semver_crate_orders_it_not_as_the_spec_says() {
        // Pinned because it is surprising and load-bearing.
        //
        // SemVer §10 says build metadata MUST be ignored when determining
        // precedence, so by the specification these are the same release. The
        // `semver` crate does not implement that rule in `Ord` — it orders by
        // build metadata too, and `1.0.0+build.2 > 1.0.0+build.1`.
        //
        // We match the crate rather than the specification *on purpose*.
        // `tauri-plugin-updater` gates updates with `release.version >
        // self.current_version` using this same crate, so implementing the spec
        // here would let the two disagree about whether a release is newer —
        // which is a policy seam of exactly the kind Gate A exists to remove.
        // If upstream's ordering ever changes, this test fails and the decision
        // gets revisited rather than silently drifting.
        assert_eq!(
            evaluate_version("1.0.0+build.1", "1.0.0+build.2"),
            VersionVerdict::Upgrade
        );
        assert!(matches!(
            refuses("1.0.0+build.2", "1.0.0+build.1"),
            Refusal::Downgrade { .. }
        ));
        // Identical strings are still the same release, which is the case that
        // actually occurs in practice.
        assert_eq!(
            evaluate_version("1.0.0+build.1", "1.0.0+build.1"),
            VersionVerdict::UpToDate
        );
    }

    #[test]
    fn a_leading_v_parses_as_tauri_parses_it() {
        // Upstream strips it, so we must too or we would refuse releases the
        // official updater accepts.
        upgrade("1.0.0", "v1.0.1");
        upgrade("v1.0.0", "v1.0.1");
    }

    #[test]
    fn an_unparseable_version_is_refused_not_ignored() {
        for (from, to, which) in [
            ("not-a-version", "1.0.0", "installed"),
            ("1.0.0", "not-a-version", "target"),
            ("1.0.0", "", "target"),
            ("1.0.0", "1.0", "target"),
        ] {
            match refuses(from, to) {
                Refusal::UncomparableVersion { which: got, .. } => {
                    assert_eq!(got, which, "{from} -> {to} should name the {which} version")
                }
                other => panic!("{from} -> {to}: expected uncomparable, got {other:?}"),
            }
        }
    }

    #[test]
    fn identity_accessors_return_what_was_supplied() {
        let identity = UpdateIdentity::new("1.0.0", "1.0.1", "linux-x86_64", "url", "sig", "{}");
        assert_eq!(identity.current_version(), "1.0.0");
        assert_eq!(identity.version(), "1.0.1");
        assert_eq!(identity.target(), "linux-x86_64");
        assert_eq!(identity.download_url(), "url");
        assert_eq!(identity.signature(), "sig");
        assert_eq!(identity.raw_json(), "{}");
    }

    #[test]
    fn refusals_describe_themselves_usefully() {
        // These reach a user or a log as the reason an update did not happen,
        // so they have to say which release was refused and why.
        let downgrade = Refusal::Downgrade {
            current: "2.0.0".to_owned(),
            target: "1.0.0".to_owned(),
        };
        let text = downgrade.to_string();
        assert!(text.contains("2.0.0") && text.contains("1.0.0"), "{text}");

        let mismatch = Refusal::IdentityMismatch {
            field: "version",
            checked: "2.0.0".to_owned(),
            delta: "1.0.0".to_owned(),
        };
        assert!(mismatch.to_string().contains("version"));
    }
}
