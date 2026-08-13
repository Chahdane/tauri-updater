//! The authenticated release identity, carried in the minisign trusted comment.
//!
//! # The problem this closes
//!
//! A minisign signature proves *these bytes were signed by the release key*. It
//! proves nothing about **which release they are**, because the release document
//! is not signed at all (`docs/DECISIONS.md` #11, finding F4). So an attacker who
//! controls the update server but holds no key can serve a genuinely signed
//! **1.0.0** artifact while the metadata says **9.9.9**: every cryptographic
//! check passes, and the user is silently rolled back to a known-vulnerable
//! build.
//!
//! # The mechanism, which already existed
//!
//! Minisign signatures carry a *trusted comment*, and it is not decoration. The
//! signature block holds two Ed25519 signatures under the same key:
//!
//! ```text
//! artifact_sig = Ed25519(sk, BLAKE2b-512(artifact bytes))
//! global_sig   = Ed25519(sk, artifact_sig ‖ trusted_comment)
//! ```
//!
//! The second **binds the comment to that specific artifact signature**, and
//! `minisign-verify` checks both inside `PublicKey::verify`
//! (`minisign-verify-0.2.5/src/lib.rs:334`) — which is the call
//! `tauri-plugin-updater` already makes, and the one this crate reproduces. The
//! comment has therefore been authenticated all along; nobody was reading it.
//!
//! Writing the release identity there means the signature stops saying only
//! *"these bytes are ours"* and starts saying *"these bytes are ours, and they
//! are version 1.0.1 of this application"*.
//!
//! Measured rather than assumed — see `research/experiments/2026-08-14-minisign-trusted-comment-binding`:
//! editing the version in the comment, splicing another release's comment onto
//! this artifact's signature, and swapping whole signature blocks all fail
//! verification.
//!
//! # What is *not* solved
//!
//! Three different properties, and conflating them is how this class of bug
//! survives:
//!
//! | Property | Status |
//! | --- | --- |
//! | **Artifact authenticity** — these bytes were signed by the key | Was already true |
//! | **Release identity** — these bytes *are* release X of app Y | **This module** |
//! | **Freshness** — release X is the newest one published | **Still not proven** |
//!
//! An attacker can still serve a *genuine older release carrying its own genuine
//! old version*. That is refused for an existing installation by the downgrade
//! policy in [`crate::identity`], and is **not** refused for a first install,
//! because nothing here proves what "latest" means. Doing so needs expiry or
//! timestamping and is outside v0.1.
//!
//! # Absence is authenticated too
//!
//! Because the tag lives inside signed bytes, "this release carries no binding"
//! cannot be *forged* either. An attacker cannot strip the identity from a bound
//! release to force the legacy path — removing it invalidates the global
//! signature. Legacy is a property of the release, not a claim by the server.

use std::fmt;

use crate::FileHash;

/// Wire tag for this version of the format. Fixed, and checked byte for byte.
pub const PROTOCOL_V1: &str = "delta-v1";

/// Prefix identifying *this family* of formats, whatever the version.
///
/// A comment starting with this and failing to parse is a contradiction and
/// fails closed. A comment not starting with it is a legacy release.
pub const PROTOCOL_FAMILY: &str = "delta-v";

/// Representation identifier for an artifact with no structured inner layer.
///
/// Named rather than omitted: the field is mandatory, so "nothing to say here"
/// needs a spelling, and an optional field in security metadata is a field an
/// attacker gets to choose the presence of.
pub const REPRESENTATION_OPAQUE_V1: &str = "opaque-v1";

/// The platform string this build runs on, in Tauri's naming.
///
/// Mirrors upstream's `updater_os()` — which maps `macos` to `darwin` — joined
/// to the architecture, because that is the vocabulary the release process uses
/// for `--platform` and therefore what a signature says. It is a **local** fact:
/// nothing a server sends can change it, which is exactly what makes it usable
/// as the other side of the comparison. See `docs/DECISIONS.md` #22 for the
/// naming confusion this deliberately does not repeat.
pub fn current_platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// Longest trusted comment this will look at.
///
/// The comment arrives inside a signature block from an untrusted document, and
/// is parsed *before* it has been authenticated in the planning pre-check. A
/// bound stops that parse being a place to spend arbitrary work.
const MAX_COMMENT_BYTES: usize = 512;

/// Why an authenticated identity could not be accepted.
///
/// Every variant means **fail closed**. None of them is a fallback: a release
/// that carries a binding and contradicts it is not a release with a transport
/// problem, it is a release whose signed description does not match what it is
/// being presented as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The comment names a protocol this build does not implement.
    UnsupportedProtocol(String),
    /// The comment is our format and does not parse.
    Malformed(String),
    /// A field's authenticated value is not the value being installed.
    Mismatch {
        /// Which field disagreed.
        field: &'static str,
        /// What the signature says.
        signed: String,
        /// What the client was about to act on.
        actual: String,
    },
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProtocol(tag) => write!(
                f,
                "release identity protocol {tag:?} is not implemented by this build"
            ),
            Self::Malformed(why) => write!(f, "malformed release identity: {why}"),
            Self::Mismatch {
                field,
                signed,
                actual,
            } => write!(
                f,
                "release identity mismatch on {field}: signed {signed:?}, actual {actual:?}"
            ),
        }
    }
}

/// A release identity, authenticated by the signature that carried it.
///
/// Obtainable only by parsing a trusted comment out of a signature — and, on the
/// authoritative path, only from a signature that has already verified. See
/// [`crate::signature::VerifiedArtifact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    /// Application bundle identifier, e.g. `dev.example.app`.
    pub app_id: String,
    /// Release version, strict semver.
    pub version: String,
    /// Canonical `{os}-{arch}`, e.g. `darwin-aarch64`.
    pub platform: String,
    /// What the artifact is, e.g. `app-tar-gz-v1` or [`REPRESENTATION_OPAQUE_V1`].
    pub representation: String,
    /// BLAKE3 of the final artifact, lowercase hex.
    pub artifact_blake3: String,
    /// Size of the final artifact in bytes.
    pub artifact_size: u64,
    /// Unix timestamp the release was signed at.
    ///
    /// Authenticated, and deliberately **not compared against anything**. It is
    /// provenance, not a freshness proof — see the module docs.
    pub signed_at: u64,
}

/// Whether a release carries an authenticated identity.
///
/// Both arms are authenticated facts about the release, because the comment they
/// are read from is covered by the global signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseBinding {
    /// The release states its identity, and the signature covers that statement.
    Authenticated(Box<ReleaseIdentity>),
    /// The release states no identity.
    ///
    /// Signed by a toolchain that predates this format — `tauri signer sign`
    /// has no flag for it. The full-download path stays available, exactly as a
    /// stock Tauri client would have it; the delta paths do not, because they
    /// are ours and can require the stronger property.
    Legacy,
}

impl ReleaseBinding {
    /// The identity, if there is one.
    pub fn identity(&self) -> Option<&ReleaseIdentity> {
        match self {
            Self::Authenticated(id) => Some(id),
            Self::Legacy => None,
        }
    }

    /// Whether the delta paths may be attempted at all.
    pub fn permits_delta(&self) -> bool {
        matches!(self, Self::Authenticated(_))
    }
}

impl ReleaseIdentity {
    /// Render the canonical wire form.
    ///
    /// The exact bytes the signature covers. Round-trips through
    /// [`parse_trusted_comment`].
    pub fn to_trusted_comment(&self) -> String {
        format!(
            "{PROTOCOL_V1} app:{} v:{} plat:{} rep:{} b3:{} sz:{} ts:{}",
            self.app_id,
            self.version,
            self.platform,
            self.representation,
            self.artifact_blake3,
            self.artifact_size,
            self.signed_at,
        )
    }

    /// Check this identity against what the client is about to install.
    ///
    /// `artifact_blake3` and `artifact_size` must describe the bytes actually in
    /// hand, not the manifest's claims about them — otherwise this compares two
    /// unauthenticated numbers to each other and proves nothing.
    pub fn check(
        &self,
        app_id: &str,
        version: &str,
        platform: &str,
        artifact_blake3: &str,
        artifact_size: u64,
    ) -> Result<(), IdentityError> {
        let mismatch = |field, signed: &str, actual: &str| IdentityError::Mismatch {
            field,
            signed: signed.to_owned(),
            actual: actual.to_owned(),
        };

        if self.app_id != app_id {
            return Err(mismatch("app", &self.app_id, app_id));
        }
        // Compared as strings, not as parsed semver. `1.0.1` and `1.0.1+build`
        // are different releases even where an ordering says otherwise, and the
        // question here is identity, not precedence.
        if self.version != version {
            return Err(mismatch("version", &self.version, version));
        }
        if self.platform != platform {
            return Err(mismatch("platform", &self.platform, platform));
        }
        if self.artifact_blake3 != artifact_blake3 {
            return Err(mismatch("blake3", &self.artifact_blake3, artifact_blake3));
        }
        if self.artifact_size != artifact_size {
            return Err(mismatch(
                "size",
                &self.artifact_size.to_string(),
                &artifact_size.to_string(),
            ));
        }
        Ok(())
    }

    /// Check that the artifact really is the representation a tar layer claims.
    ///
    /// The manifest's `tar_layer.representation` is unauthenticated. This is
    /// what stops it describing an artifact as something it is not.
    pub fn check_representation(&self, representation: &str) -> Result<(), IdentityError> {
        if self.representation != representation {
            return Err(IdentityError::Mismatch {
                field: "representation",
                signed: self.representation.clone(),
                actual: representation.to_owned(),
            });
        }
        Ok(())
    }
}

/// Parse a minisign trusted comment.
///
/// `Ok(Legacy)` means the comment is not this format at all. Anything beginning
/// [`PROTOCOL_FAMILY`] must parse as a version this build implements, or it is
/// an error — a release cannot half-claim a binding.
///
/// # Strictness
///
/// Deliberately unforgiving, because a permissive parser for security metadata
/// is a way for two readers to disagree about what was signed:
///
/// - ASCII only, no control characters
/// - exactly the eight expected tokens, in order, separated by single spaces
/// - no leading or trailing whitespace, no tabs, no repeated separators
/// - every key present exactly once, no unknown keys, nothing ignored
/// - strict semver that re-serialises to itself
/// - BLAKE3 as exactly 64 lowercase hex characters
/// - decimals with no sign, no leading zeros, no underscores
pub fn parse_trusted_comment(comment: &str) -> Result<ReleaseBinding, IdentityError> {
    if !comment.starts_with(PROTOCOL_FAMILY) {
        return Ok(ReleaseBinding::Legacy);
    }
    if comment.len() > MAX_COMMENT_BYTES {
        return Err(IdentityError::Malformed(format!(
            "comment is {} bytes, over the {MAX_COMMENT_BYTES} byte limit",
            comment.len()
        )));
    }
    if !comment.is_ascii() {
        return Err(IdentityError::Malformed("comment is not ASCII".into()));
    }
    if comment.bytes().any(|b| b.is_ascii_control()) {
        return Err(IdentityError::Malformed(
            "comment contains a control character".into(),
        ));
    }

    // `split(' ')` rather than `split_whitespace()`: the latter silently accepts
    // tabs, runs of spaces and surrounding whitespace, which would make several
    // different byte strings parse identically. Canonical means one spelling.
    let tokens: Vec<&str> = comment.split(' ').collect();

    let tag = tokens[0];
    if tag != PROTOCOL_V1 {
        return Err(IdentityError::UnsupportedProtocol(tag.to_owned()));
    }
    if tokens.len() != 8 {
        return Err(IdentityError::Malformed(format!(
            "expected 8 space-separated fields, found {}",
            tokens.len()
        )));
    }

    let field = |index: usize, key: &str| -> Result<&str, IdentityError> {
        let token = tokens[index];
        token.strip_prefix(key).ok_or_else(|| {
            IdentityError::Malformed(format!("field {index} should start with {key:?}"))
        })
    };

    let app_id = field(1, "app:")?;
    let version = field(2, "v:")?;
    let platform = field(3, "plat:")?;
    let representation = field(4, "rep:")?;
    let blake3 = field(5, "b3:")?;
    let size = field(6, "sz:")?;
    let timestamp = field(7, "ts:")?;

    check_app_id(app_id)?;
    check_version(version)?;
    check_platform(platform)?;
    check_token("representation", representation)?;
    check_blake3(blake3)?;

    Ok(ReleaseBinding::Authenticated(Box::new(ReleaseIdentity {
        app_id: app_id.to_owned(),
        version: version.to_owned(),
        platform: platform.to_owned(),
        representation: representation.to_owned(),
        artifact_blake3: blake3.to_owned(),
        artifact_size: check_decimal("size", size)?,
        signed_at: check_decimal("timestamp", timestamp)?,
    })))
}

fn check_app_id(value: &str) -> Result<(), IdentityError> {
    if value.is_empty() || value.len() > 155 {
        return Err(IdentityError::Malformed(format!(
            "app identifier length {} is out of range",
            value.len()
        )));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(IdentityError::Malformed(
            "app identifier has characters outside [A-Za-z0-9._-]".into(),
        ));
    }
    Ok(())
}

fn check_version(value: &str) -> Result<(), IdentityError> {
    let parsed = semver::Version::parse(value)
        .map_err(|e| IdentityError::Malformed(format!("version {value:?} is not semver: {e}")))?;
    // Round-trip, so `1.0.01` or other non-canonical spellings of the same
    // version cannot be signed as one thing and compared as another.
    if parsed.to_string() != value {
        return Err(IdentityError::Malformed(format!(
            "version {value:?} is not canonical semver (canonical form is {parsed})"
        )));
    }
    Ok(())
}

fn check_platform(value: &str) -> Result<(), IdentityError> {
    let Some((os, arch)) = value.split_once('-') else {
        return Err(IdentityError::Malformed(format!(
            "platform {value:?} is not {{os}}-{{arch}}"
        )));
    };
    for part in [os, arch] {
        if part.is_empty()
            || !part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(IdentityError::Malformed(format!(
                "platform {value:?} has a component outside [a-z0-9_]"
            )));
        }
    }
    Ok(())
}

fn check_token(what: &str, value: &str) -> Result<(), IdentityError> {
    if value.is_empty() || value.len() > 64 {
        return Err(IdentityError::Malformed(format!(
            "{what} length {} is out of range",
            value.len()
        )));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_'))
    {
        return Err(IdentityError::Malformed(format!(
            "{what} has characters outside [a-z0-9-._]"
        )));
    }
    Ok(())
}

fn check_blake3(value: &str) -> Result<(), IdentityError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(IdentityError::Malformed(
            "blake3 must be exactly 64 lowercase hex characters".into(),
        ));
    }
    // Belt and braces: the shared parser is the thing every other digest in this
    // crate goes through, so it decides what a digest is.
    FileHash::from_hex(value)
        .map_err(|e| IdentityError::Malformed(format!("blake3 is not a valid digest: {e}")))?;
    Ok(())
}

fn check_decimal(what: &str, value: &str) -> Result<u64, IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::Malformed(format!("{what} is empty")));
    }
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(IdentityError::Malformed(format!(
            "{what} {value:?} is not a plain decimal"
        )));
    }
    // "0" is the only value that may begin with a zero, and a zero-byte artifact
    // is not a release, so it is rejected below anyway.
    if value.len() > 1 && value.starts_with('0') {
        return Err(IdentityError::Malformed(format!(
            "{what} {value:?} has a leading zero"
        )));
    }
    let parsed: u64 = value
        .parse()
        .map_err(|_| IdentityError::Malformed(format!("{what} {value:?} does not fit in u64")))?;
    if what == "size" && parsed == 0 {
        return Err(IdentityError::Malformed("size is zero".into()));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ReleaseIdentity {
        ReleaseIdentity {
            app_id: "dev.chahdane.delta-updater-example".to_owned(),
            version: "1.0.1".to_owned(),
            platform: "darwin-aarch64".to_owned(),
            representation: "app-tar-gz-v1".to_owned(),
            artifact_blake3: "9230ca75c50638fed5e8e17b895dac5c54e73e3535301b1615a0943a0f22a51d"
                .to_owned(),
            artifact_size: 4_163_366,
            signed_at: 1_786_637_312,
        }
    }

    fn parse(comment: &str) -> Result<ReleaseBinding, IdentityError> {
        parse_trusted_comment(comment)
    }

    fn parsed(comment: &str) -> ReleaseIdentity {
        match parse(comment).expect("should parse") {
            ReleaseBinding::Authenticated(id) => *id,
            ReleaseBinding::Legacy => panic!("expected an authenticated identity"),
        }
    }

    #[test]
    fn the_canonical_form_round_trips() {
        let original = identity();
        let comment = original.to_trusted_comment();
        assert_eq!(
            comment,
            "delta-v1 app:dev.chahdane.delta-updater-example v:1.0.1 plat:darwin-aarch64 \
             rep:app-tar-gz-v1 \
             b3:9230ca75c50638fed5e8e17b895dac5c54e73e3535301b1615a0943a0f22a51d \
             sz:4163366 ts:1786637312"
                .replace("             ", "")
                .replace("  ", " ")
        );
        assert_eq!(parsed(&comment), original);
    }

    #[test]
    fn the_wire_format_is_pinned_byte_for_byte() {
        // The bytes a signature covers. Changing them is a protocol change and
        // must be a new version tag, not a silent edit — so the expected string
        // is written out in full rather than derived.
        let expected = concat!(
            "delta-v1 app:dev.chahdane.delta-updater-example v:1.0.1 ",
            "plat:darwin-aarch64 rep:app-tar-gz-v1 ",
            "b3:9230ca75c50638fed5e8e17b895dac5c54e73e3535301b1615a0943a0f22a51d ",
            "sz:4163366 ts:1786637312"
        );
        assert_eq!(identity().to_trusted_comment(), expected);
    }

    #[test]
    fn a_comment_from_another_toolchain_is_legacy_not_an_error() {
        // What `tauri signer sign` and the minisign crate write by default.
        assert_eq!(
            parse("timestamp:1786637312").expect("legacy"),
            ReleaseBinding::Legacy
        );
        assert_eq!(parse("").expect("legacy"), ReleaseBinding::Legacy);
        assert_eq!(
            parse("timestamp:1786637312\tfile:App.app.tar.gz").expect("legacy"),
            ReleaseBinding::Legacy
        );
        assert!(!ReleaseBinding::Legacy.permits_delta());
    }

    #[test]
    fn a_future_protocol_version_fails_closed_rather_than_reading_as_legacy() {
        // The dangerous mistake would be treating anything unparseable as
        // "no binding", because that is the attacker's preferred outcome: it
        // downgrades a bound release to the legacy rules.
        let err = parse("delta-v2 app:a v:1.0.0 plat:darwin-aarch64 rep:opaque-v1 b3:00 sz:1 ts:1")
            .expect_err("must not be treated as legacy");
        assert!(
            matches!(&err, IdentityError::UnsupportedProtocol(tag) if tag == "delta-v2"),
            "got {err:?}"
        );
    }

    #[test]
    fn our_family_with_a_broken_body_fails_closed() {
        for broken in [
            "delta-v1",
            "delta-v1 app:a",
            "delta-v1 app:a v:1.0.0 plat:darwin-aarch64 rep:opaque-v1 b3:00 sz:1",
            "delta-v1 app:a v:1.0.0 plat:darwin-aarch64 rep:opaque-v1 b3:00 sz:1 ts:1 extra:x",
        ] {
            assert!(
                matches!(parse(broken), Err(IdentityError::Malformed(_))),
                "{broken:?} should have been rejected"
            );
        }
    }

    #[test]
    fn fields_must_be_in_the_declared_order_with_single_spaces() {
        let good = identity().to_trusted_comment();
        // Reordered.
        let swapped = good.replace(
            "app:dev.chahdane.delta-updater-example v:1.0.1",
            "v:1.0.1 app:dev.chahdane.delta-updater-example",
        );
        assert!(matches!(parse(&swapped), Err(IdentityError::Malformed(_))));

        // Doubled separator, and leading/trailing space. Each is a different
        // byte string that a whitespace-splitting parser would accept as equal.
        assert!(matches!(
            parse(&good.replace("app:", " app:")),
            Err(IdentityError::Malformed(_))
        ));
        assert!(matches!(
            parse(&format!("{good} ")),
            Err(IdentityError::Malformed(_))
        ));
        assert!(matches!(
            parse(&good.replacen(' ', "\t", 1)),
            Err(IdentityError::Malformed(_))
        ));
    }

    #[test]
    fn a_duplicate_field_cannot_shadow_an_earlier_one() {
        // The classic parser-differential: two `v:` fields where one reader
        // takes the first and another takes the last.
        let good = identity().to_trusted_comment();
        let doubled = good.replace("v:1.0.1", "v:1.0.1 v:9.9.9");
        assert!(matches!(parse(&doubled), Err(IdentityError::Malformed(_))));
    }

    #[test]
    fn rejects_non_canonical_field_values() {
        let base = identity();
        let cases: Vec<(&str, String)> = vec![
            (
                "non-canonical semver",
                base.to_trusted_comment().replace("v:1.0.1", "v:1.0.01"),
            ),
            (
                "not semver",
                base.to_trusted_comment().replace("v:1.0.1", "v:1.0"),
            ),
            // Only the digest, not the whole comment: uppercasing the tag too
            // would make it stop matching PROTOCOL_FAMILY and read as legacy,
            // which is correct behaviour and would test nothing here.
            (
                "uppercase digest",
                base.to_trusted_comment()
                    .replace(&base.artifact_blake3, &base.artifact_blake3.to_uppercase()),
            ),
            (
                "short digest",
                base.to_trusted_comment()
                    .replace(&base.artifact_blake3, "abcd"),
            ),
            (
                "size leading zero",
                base.to_trusted_comment()
                    .replace("sz:4163366", "sz:04163366"),
            ),
            (
                "size not decimal",
                base.to_trusted_comment()
                    .replace("sz:4163366", "sz:4_163_366"),
            ),
            (
                "negative size",
                base.to_trusted_comment().replace("sz:4163366", "sz:-1"),
            ),
            (
                "zero size",
                base.to_trusted_comment().replace("sz:4163366", "sz:0"),
            ),
            (
                "platform without arch",
                base.to_trusted_comment()
                    .replace("plat:darwin-aarch64", "plat:darwin"),
            ),
            (
                "uppercase platform",
                base.to_trusted_comment()
                    .replace("plat:darwin-aarch64", "plat:Darwin-aarch64"),
            ),
            (
                "empty app id",
                base.to_trusted_comment()
                    .replace("app:dev.chahdane.delta-updater-example", "app:"),
            ),
            (
                "app id with a space is a field-count error",
                base.to_trusted_comment()
                    .replace("app:dev.chahdane.delta-updater-example", "app:dev example"),
            ),
        ];
        for (name, comment) in cases {
            assert!(
                parse(&comment).is_err(),
                "{name}: {comment:?} should have been rejected"
            );
        }
    }

    #[test]
    fn a_non_ascii_comment_is_refused() {
        let sneaky = identity()
            .to_trusted_comment()
            .replace("1.0.1", "1.0.\u{0661}");
        assert!(matches!(parse(&sneaky), Err(IdentityError::Malformed(_))));
    }

    #[test]
    fn an_oversized_comment_is_refused_before_it_is_split() {
        let huge = format!("delta-v1 {}", "a".repeat(MAX_COMMENT_BYTES));
        assert!(matches!(parse(&huge), Err(IdentityError::Malformed(_))));
    }

    // ---- the comparisons themselves --------------------------------------

    #[test]
    fn an_honest_identity_checks_out() {
        let id = identity();
        id.check(
            &id.app_id,
            &id.version,
            &id.platform,
            &id.artifact_blake3,
            id.artifact_size,
        )
        .expect("an identity must accept the release it describes");
        id.check_representation(&id.representation).expect("same");
    }

    #[test]
    fn each_field_is_compared_and_reported_by_name() {
        let id = identity();
        let cases: Vec<(&str, Box<dyn Fn() -> Result<(), IdentityError>>)> = vec![
            (
                "app",
                Box::new(|| {
                    let id = identity();
                    id.check(
                        "dev.other.app",
                        &id.version,
                        &id.platform,
                        &id.artifact_blake3,
                        id.artifact_size,
                    )
                }),
            ),
            (
                "version",
                Box::new(|| {
                    let id = identity();
                    id.check(
                        &id.app_id,
                        "9.9.9",
                        &id.platform,
                        &id.artifact_blake3,
                        id.artifact_size,
                    )
                }),
            ),
            (
                "platform",
                Box::new(|| {
                    let id = identity();
                    id.check(
                        &id.app_id,
                        &id.version,
                        "linux-x86_64",
                        &id.artifact_blake3,
                        id.artifact_size,
                    )
                }),
            ),
            (
                "blake3",
                Box::new(|| {
                    let id = identity();
                    id.check(
                        &id.app_id,
                        &id.version,
                        &id.platform,
                        &"aa".repeat(32),
                        id.artifact_size,
                    )
                }),
            ),
            (
                "size",
                Box::new(|| {
                    let id = identity();
                    id.check(
                        &id.app_id,
                        &id.version,
                        &id.platform,
                        &id.artifact_blake3,
                        1,
                    )
                }),
            ),
        ];
        for (field, run) in cases {
            match run() {
                Err(IdentityError::Mismatch { field: got, .. }) => assert_eq!(got, field),
                other => panic!("{field} should have mismatched, got {other:?}"),
            }
        }
        assert!(matches!(
            id.check_representation("appimage-v1"),
            Err(IdentityError::Mismatch {
                field: "representation",
                ..
            })
        ));
    }

    #[test]
    fn a_build_metadata_variant_is_a_different_release() {
        // semver orders `1.0.1` and `1.0.1+b` as distinct, and even where an
        // ordering said otherwise these are different artifacts. Identity is not
        // precedence.
        let id = identity();
        assert!(id
            .check(
                &id.app_id,
                "1.0.1+build.2",
                &id.platform,
                &id.artifact_blake3,
                id.artifact_size
            )
            .is_err());
    }
}
