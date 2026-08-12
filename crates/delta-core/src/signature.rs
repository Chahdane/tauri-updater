//! Signature verification, and a token that can only exist because it happened.
//!
//! Tauri does not verify what you hand it. `tauri-plugin-updater` calls
//! `verify_signature` from exactly one place — inside `Update::download()` — and
//! the handoff seam, `Update::install(bytes)`, goes straight to the installer.
//! The delta path exists to avoid that download, so this plugin's own check is
//! the *only* thing standing between a reconstructed artifact and installation.
//! See `docs/DECISIONS.md` #10.
//!
//! A sole gate with no backstop should not depend on every call site remembering
//! to pass through it. So it is enforced by the type system instead:
//! [`VerifiedArtifact`] has a private field and no public constructor, which
//! means [`verify_artifact`] is the only way in the language to obtain one. A
//! handoff that accepts `&VerifiedArtifact` cannot be called with unverified
//! bytes — not by a mistake, not by a future refactor, and not by a second entry
//! point someone adds later without reading this file.
//!
//! This is the same move as [`Reconstruction`](crate::Reconstruction) making the
//! fallback unavoidable: put the invariant where the compiler enforces it rather
//! than where a reviewer has to notice it.
//!
//! # Why bytes rather than a path
//!
//! The token owns the bytes that were verified, not a path to them. A path would
//! leave a window between the check and the install in which the file could be
//! replaced, and the signature would then describe something other than what
//! gets installed. Holding the bytes closes it: what was verified and what is
//! handed over are the same allocation.
//!
//! The cost is keeping the artifact resident. Tauri's own `download()` does
//! exactly this — `let mut buffer = Vec::new()` — so this is no worse than the
//! path it replaces.
//!
//! # Verification itself
//!
//! Reproduced from `tauri-plugin-updater` 2.10.1, `src/updater.rs:1453`, rather
//! than reimplemented, so the artifact this plugin accepts is precisely the
//! artifact Tauri would have accepted. `tauri_signature_compat.rs` pins that
//! equivalence.

use std::path::Path;

use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};

use crate::{Error, Result};

/// An artifact whose minisign signature has been verified.
///
/// The only way to construct one is [`verify_artifact`]. Take this type as a
/// parameter wherever an unverified artifact must be impossible to supply.
///
/// Deliberately not `Deserialize`, not `Default`, and carrying no public field —
/// each of those would be a way to mint the proof without doing the work.
///
/// The guarantee is the compiler's, not a convention. Forging the token does not
/// fail a review; it fails to build:
///
/// ```compile_fail
/// use tauri_updater_delta_core::VerifiedArtifact;
/// // error[E0451]: field `bytes` of struct `VerifiedArtifact` is private
/// let forged = VerifiedArtifact { bytes: b"unverified".to_vec() };
/// ```
///
/// ```compile_fail
/// use tauri_updater_delta_core::VerifiedArtifact;
/// // No Default, no From<Vec<u8>>, no other way in.
/// let forged: VerifiedArtifact = Default::default();
/// ```
///
/// And the honest limit: within *this* crate the field is reachable, so the
/// guarantee is that no code outside `delta-core` can mint one. Inside it, this
/// module is the only place that constructs the type, and that is deliberately a
/// small surface to keep under review.
#[derive(Debug, Clone)]
pub struct VerifiedArtifact {
    /// Private. This is the whole mechanism.
    bytes: Vec<u8>,
}

impl VerifiedArtifact {
    /// The verified bytes — exactly those the signature was checked against.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Size of the artifact in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the artifact is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Consume the token, yielding the bytes.
    ///
    /// Named to be conspicuous at a call site: past this point the proof is
    /// gone and the bytes are ordinary again. Hand a `&VerifiedArtifact` around
    /// instead wherever that is possible.
    pub fn into_unverified_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Verify `bytes` against a base64 minisign signature and public key.
///
/// Both encodings are Tauri's: base64 of the whole minisign file, comment line
/// included, exactly as stored in a manifest and in `tauri.conf.json`.
pub fn verify_artifact(
    bytes: Vec<u8>,
    signature_b64: &str,
    pubkey_b64: &str,
) -> Result<VerifiedArtifact> {
    // Reproduces tauri-plugin-updater's verify_signature step for step. Note
    // PublicKey::decode over the whole key file, not from_base64 over the key
    // line — Tauri does the former, and the two are easy to confuse.
    let pubkey_decoded = base64_to_string(pubkey_b64, "public key")?;
    let public_key = PublicKey::decode(&pubkey_decoded)
        .map_err(|e| Error::Signature(format!("unusable public key: {e}")))?;

    let signature_decoded = base64_to_string(signature_b64, "signature")?;
    let signature = Signature::decode(&signature_decoded)
        .map_err(|e| Error::Signature(format!("unusable signature: {e}")))?;

    public_key
        .verify(&bytes, &signature, true)
        .map_err(|e| Error::Signature(e.to_string()))?;

    Ok(VerifiedArtifact { bytes })
}

/// Read `path` and verify it.
pub fn verify_artifact_file(
    path: &Path,
    signature_b64: &str,
    pubkey_b64: &str,
) -> Result<VerifiedArtifact> {
    let bytes = std::fs::read(path).map_err(|e| Error::io("read", path, e))?;
    verify_artifact(bytes, signature_b64, pubkey_b64)
}

fn base64_to_string(value: &str, what: &str) -> Result<String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|e| Error::Signature(format!("{what} is not valid base64: {e}")))?;
    String::from_utf8(decoded)
        .map_err(|_| Error::Signature(format!("{what} did not decode to text")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed keypair and signature over `b"the released installer"`, generated
    /// once with the `minisign` crate and pinned here so this module needs no
    /// signing dependency.
    const PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXkKUldRZjYyWm5NRlZUOFRQTVh4b3Nia3RtY29ScTB2VC9kSFdTeGxWVHBmZFVKM3lWWmVDdWZlWG4K";

    fn sample() -> (Vec<u8>, String, String) {
        // Generated at test time rather than pinned, so this stays correct if
        // the encoding changes; the pinned-key path is covered by the
        // compatibility suite in the release crate.
        let pair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new()))
            .expect("generate keypair");
        let bytes = b"the released installer".to_vec();

        let signature = minisign::sign(None, &pair.sk, &bytes[..], None, None)
            .expect("sign")
            .into_string();
        let signature_b64 = base64::engine::general_purpose::STANDARD.encode(signature);

        let pubkey = pair.pk.to_box().expect("box pk").into_string();
        let pubkey_b64 = base64::engine::general_purpose::STANDARD.encode(pubkey);

        (bytes, signature_b64, pubkey_b64)
    }

    #[test]
    fn a_valid_signature_yields_a_token() {
        let (bytes, signature, pubkey) = sample();
        let verified = verify_artifact(bytes.clone(), &signature, &pubkey).expect("should verify");
        assert_eq!(verified.as_bytes(), &bytes[..]);
        assert_eq!(verified.len(), bytes.len());
        assert!(!verified.is_empty());
    }

    #[test]
    fn the_token_carries_the_bytes_that_were_checked() {
        // Not a path that could be swapped afterwards — the proof and the
        // payload are the same allocation.
        let (bytes, signature, pubkey) = sample();
        let verified = verify_artifact(bytes.clone(), &signature, &pubkey).expect("should verify");
        assert_eq!(verified.into_unverified_bytes(), bytes);
    }

    #[test]
    fn tampered_bytes_produce_no_token() {
        let (_, signature, pubkey) = sample();
        let err = verify_artifact(b"a different installer".to_vec(), &signature, &pubkey)
            .expect_err("must not verify");
        assert!(matches!(err, Error::Signature(_)), "unexpected: {err}");
    }

    #[test]
    fn another_keys_signature_produces_no_token() {
        let (bytes, signature, _) = sample();
        let (_, _, other_pubkey) = sample();
        assert!(verify_artifact(bytes, &signature, &other_pubkey).is_err());
    }

    #[test]
    fn malformed_inputs_produce_no_token() {
        let (bytes, signature, pubkey) = sample();
        assert!(verify_artifact(bytes.clone(), "not base64!", &pubkey).is_err());
        assert!(verify_artifact(bytes.clone(), &signature, "not base64!").is_err());
        assert!(verify_artifact(bytes, "", "").is_err());
    }

    #[test]
    fn a_pinned_tauri_style_public_key_decodes() {
        // Guards the encoding itself: Tauri stores base64 of the whole .pub
        // file, and PublicKey::decode expects that file's text.
        let decoded = base64_to_string(PUBKEY, "public key").expect("decode");
        assert!(decoded.starts_with("untrusted comment:"));
        PublicKey::decode(&decoded).expect("a real Tauri-style public key should parse");
    }

    #[test]
    fn verifies_from_a_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (bytes, signature, pubkey) = sample();
        let path = dir.path().join("app.AppImage");
        std::fs::write(&path, &bytes).expect("write");

        let verified = verify_artifact_file(&path, &signature, &pubkey).expect("should verify");
        assert_eq!(verified.as_bytes(), &bytes[..]);

        // Swapping the file afterwards cannot affect the token.
        std::fs::write(&path, b"swapped after verification").expect("overwrite");
        assert_eq!(verified.as_bytes(), &bytes[..]);
    }
}
