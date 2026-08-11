//! The release manifest: a superset of Tauri's static updater JSON.
//!
//! # Why a superset
//!
//! The delta path is an optimisation that must never become a dependency, which
//! means the full-download fallback has to keep working exactly as it does
//! today. The cheapest way to guarantee that is to make this document *be* the
//! official updater's manifest, with the delta information added alongside
//! rather than replacing anything.
//!
//! A Tauri client that knows nothing about deltas reads [`version`],
//! [`pub_date`] and [`platforms`] and behaves normally. A delta-aware client
//! reads the same fields, plus [`delta`]. There is no second document to publish
//! and no way for the two views to drift apart.
//!
//! [`version`]: Manifest::version
//! [`pub_date`]: Manifest::pub_date
//! [`platforms`]: Manifest::platforms
//! [`delta`]: Manifest::delta
//!
//! # Shape
//!
//! ```json
//! {
//!   "version": "1.0.1",
//!   "pub_date": "2026-08-12T10:00:00Z",
//!   "platforms": {
//!     "linux-x86_64": {
//!       "url": "https://releases.example.com/app_1.0.1.AppImage",
//!       "signature": "<minisign signature over that installer>"
//!     }
//!   },
//!   "delta": {
//!     "schema": 1,
//!     "hash_algo": "blake3",
//!     "platforms": {
//!       "linux-x86_64": {
//!         "target_version": "1.0.1",
//!         "target_installer_blake3": "…",
//!         "target_installer_size": 6815744,
//!         "signature": "<the same minisign signature>",
//!         "patches": {
//!           "1.0.0": {
//!             "backend_id": "zstd",
//!             "patch_url": "https://releases.example.com/1.0.0-to-1.0.1.zst",
//!             "patch_blake3": "…",
//!             "patch_size": 393782
//!           }
//!         }
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! # What the signature covers
//!
//! [`signature`](DeltaPlatform::signature) is over the **target installer** —
//! the artifact that actually gets installed — not over the patch. Both paths
//! converge on the same file, so both validate against the same signature, and
//! the delta path cannot install anything the full-download path would have
//! rejected.
//!
//! Hash fields carry their algorithm in the name, and [`DeltaLayer::hash_algo`]
//! states it explicitly, so a client can refuse a document it cannot check
//! instead of comparing a BLAKE3 digest against a SHA-256 field.
//!
//! # Version coverage
//!
//! Schema 1 is patch-from-any-previous-to-latest: every entry in
//! [`patches`](DeltaPlatform::patches) reconstructs the *current* release. There
//! are no multi-hop chains, so a client is never asked to apply two patches in
//! sequence. Chaining is future work and would arrive as `schema: 2`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Error, FileHash, Result};

/// Schema version this build writes and understands.
pub const SCHEMA_VERSION: u32 = 1;

/// Name recorded in [`DeltaLayer::hash_algo`] for the digests this crate produces.
pub const HASH_ALGO: &str = "blake3";

/// A release manifest: Tauri's static updater document plus a delta layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Version being released, e.g. `"1.0.1"`.
    pub version: String,

    /// Release notes, surfaced by Tauri's updater.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    /// RFC 3339 publication timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_date: Option<String>,

    /// Full-download targets, keyed by Tauri's `{os}-{arch}` identifier.
    ///
    /// This is the fallback path, and it is the official updater's own field —
    /// untouched, so a delta-unaware client works against this document as-is.
    pub platforms: BTreeMap<String, TauriPlatform>,

    /// Delta information. Absent means no patches are published for this release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<DeltaLayer>,
}

/// One full-download target, in Tauri's own format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TauriPlatform {
    /// Where the complete installer artifact can be downloaded.
    pub url: String,
    /// Base64 minisign signature over the installer at `url`.
    pub signature: String,
}

/// The delta layer added on top of Tauri's document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaLayer {
    /// Schema version; a client refuses anything it does not implement.
    pub schema: u32,
    /// Digest algorithm used by every hash field below.
    pub hash_algo: String,
    /// Delta targets, keyed by the same `{os}-{arch}` identifier as `platforms`.
    pub platforms: BTreeMap<String, DeltaPlatform>,
}

/// Delta information for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaPlatform {
    /// Version every patch here reconstructs. Matches [`Manifest::version`].
    pub target_version: String,

    /// Digest of the installer a patch must reproduce.
    ///
    /// This is the gate: a reconstruction that does not hash to this value is
    /// discarded and the full artifact downloaded instead.
    pub target_installer_blake3: String,

    /// Size in bytes of that installer.
    ///
    /// Used to bound decompression before it starts — both the output written
    /// and the window zstd is permitted to allocate — so a hostile patch cannot
    /// force an unbounded allocation.
    pub target_installer_size: u64,

    /// Base64 minisign signature over the target installer.
    ///
    /// Deliberately the same value as the matching [`TauriPlatform::signature`],
    /// because both paths install the same artifact. [`Manifest::validate`]
    /// rejects a document where the two disagree.
    pub signature: String,

    /// Patches into this release, keyed by the version being upgraded *from*.
    pub patches: BTreeMap<String, Patch>,
}

/// A single patch, from one previous version to the release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    /// Backend that produced it, e.g. `"zstd"`. A client that does not have this
    /// backend declines the patch and falls back.
    pub backend_id: String,
    /// Where the patch can be downloaded.
    pub patch_url: String,
    /// Digest of the patch file itself, checked before it is applied.
    pub patch_blake3: String,
    /// Size of the patch in bytes.
    pub patch_size: u64,
}

impl Manifest {
    /// Parse a manifest and check its invariants.
    pub fn from_json(json: &str) -> Result<Self> {
        let manifest: Self =
            serde_json::from_str(json).map_err(|e| Error::Manifest(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Serialize as pretty-printed JSON.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| Error::Manifest(e.to_string()))
    }

    /// Check the invariants that keep the two layers describing the same release.
    ///
    /// The delta layer duplicates information the Tauri layer already carries.
    /// That redundancy is deliberate — it keeps each layer independently
    /// readable — but redundancy that is never checked is just an opportunity to
    /// disagree, so it is checked here on every load.
    pub fn validate(&self) -> Result<()> {
        let Some(delta) = &self.delta else {
            return Ok(());
        };

        if delta.schema != SCHEMA_VERSION {
            return Err(Error::Manifest(format!(
                "unsupported delta schema {}, this build implements {SCHEMA_VERSION}",
                delta.schema
            )));
        }

        if delta.hash_algo != HASH_ALGO {
            return Err(Error::Manifest(format!(
                "manifest hashes are {:?} but this build computes {HASH_ALGO:?}",
                delta.hash_algo
            )));
        }

        for (platform, entry) in &delta.platforms {
            if entry.target_version != self.version {
                return Err(Error::Manifest(format!(
                    "delta entry for {platform} targets {} but the release is {}",
                    entry.target_version, self.version
                )));
            }

            // A platform with patches but no full-download target would leave a
            // client with nothing to fall back to.
            let Some(tauri) = self.platforms.get(platform) else {
                return Err(Error::Manifest(format!(
                    "delta entry for {platform} has no matching full-download target"
                )));
            };

            if tauri.signature != entry.signature {
                return Err(Error::Manifest(format!(
                    "signature mismatch for {platform}: the delta and full-download \
                     layers describe the same installer and must carry the same signature"
                )));
            }

            // Fail on a malformed digest here rather than after a patch has been
            // downloaded and applied.
            FileHash::from_hex(&entry.target_installer_blake3)?;

            for (from, patch) in &entry.patches {
                FileHash::from_hex(&patch.patch_blake3)?;
                if from == &entry.target_version {
                    return Err(Error::Manifest(format!(
                        "delta entry for {platform} patches {from} to itself"
                    )));
                }
            }
        }

        Ok(())
    }

    /// Look up the patch that upgrades `from_version` to this release on
    /// `platform`, along with the platform's delta metadata.
    ///
    /// Returns `None` whenever the delta path is not available — no delta layer,
    /// no entry for the platform, or no patch from that version. Every `None` is
    /// ordinary and means "download the full artifact".
    pub fn patch_for(
        &self,
        platform: &str,
        from_version: &str,
    ) -> Option<(&DeltaPlatform, &Patch)> {
        let delta = self.delta.as_ref()?;
        let entry = delta.platforms.get(platform)?;
        let patch = entry.patches.get(from_version)?;
        Some((entry, patch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature() -> String {
        "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZQo=".to_owned()
    }

    fn digest() -> String {
        FileHash::of_bytes(b"installer").to_hex()
    }

    fn manifest() -> Manifest {
        Manifest {
            version: "1.0.1".to_owned(),
            notes: Some("Fixes".to_owned()),
            pub_date: Some("2026-08-12T10:00:00Z".to_owned()),
            platforms: BTreeMap::from([(
                "linux-x86_64".to_owned(),
                TauriPlatform {
                    url: "https://example.com/app_1.0.1.AppImage".to_owned(),
                    signature: signature(),
                },
            )]),
            delta: Some(DeltaLayer {
                schema: SCHEMA_VERSION,
                hash_algo: HASH_ALGO.to_owned(),
                platforms: BTreeMap::from([(
                    "linux-x86_64".to_owned(),
                    DeltaPlatform {
                        target_version: "1.0.1".to_owned(),
                        target_installer_blake3: digest(),
                        target_installer_size: 6_815_744,
                        signature: signature(),
                        patches: BTreeMap::from([(
                            "1.0.0".to_owned(),
                            Patch {
                                backend_id: "zstd".to_owned(),
                                patch_url: "https://example.com/1.0.0-to-1.0.1.zst".to_owned(),
                                patch_blake3: FileHash::of_bytes(b"patch").to_hex(),
                                patch_size: 393_782,
                            },
                        )]),
                    },
                )]),
            }),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let original = manifest();
        let parsed = Manifest::from_json(&original.to_json().expect("serialize")).expect("parse");
        assert_eq!(original, parsed);
    }

    #[test]
    fn a_delta_unaware_client_sees_a_normal_tauri_manifest() {
        // The whole point of the superset: strip our layer and what remains is
        // exactly the document the official updater already consumes.
        let json = manifest().to_json().expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse as json");

        assert_eq!(value["version"], "1.0.1");
        assert_eq!(value["pub_date"], "2026-08-12T10:00:00Z");
        let platform = &value["platforms"]["linux-x86_64"];
        assert_eq!(platform["url"], "https://example.com/app_1.0.1.AppImage");
        assert_eq!(platform["signature"], signature());
        assert!(
            platform.get("patch_url").is_none(),
            "delta fields must not leak into the Tauri layer"
        );
    }

    #[test]
    fn parses_a_manifest_with_no_delta_layer() {
        let json = r#"{
            "version": "1.0.1",
            "platforms": {
                "linux-x86_64": { "url": "https://example.com/a", "signature": "sig" }
            }
        }"#;
        let manifest = Manifest::from_json(json).expect("a delta-free manifest is valid");
        assert!(manifest.delta.is_none());
        assert!(manifest.patch_for("linux-x86_64", "1.0.0").is_none());
    }

    #[test]
    fn finds_the_patch_for_an_installed_version() {
        let manifest = manifest();
        let (entry, patch) = manifest
            .patch_for("linux-x86_64", "1.0.0")
            .expect("patch should be listed");
        assert_eq!(entry.target_version, "1.0.1");
        assert_eq!(patch.backend_id, "zstd");
    }

    #[test]
    fn absent_patches_are_not_errors() {
        let manifest = manifest();
        // Every one of these means "download the full artifact", not "fail".
        assert!(manifest.patch_for("linux-x86_64", "0.9.0").is_none());
        assert!(manifest.patch_for("windows-x86_64", "1.0.0").is_none());
    }

    #[test]
    fn rejects_a_future_schema() {
        let mut manifest = manifest();
        manifest.delta.as_mut().unwrap().schema = SCHEMA_VERSION + 1;
        assert!(matches!(manifest.validate(), Err(Error::Manifest(_))));
    }

    #[test]
    fn rejects_a_hash_algorithm_it_cannot_compute() {
        let mut manifest = manifest();
        manifest.delta.as_mut().unwrap().hash_algo = "sha256".to_owned();
        let err = manifest.validate().expect_err("should reject");
        assert!(
            err.to_string().contains("sha256"),
            "the error should name the algorithm: {err}"
        );
    }

    #[test]
    fn rejects_disagreeing_signatures() {
        let mut manifest = manifest();
        manifest
            .delta
            .as_mut()
            .unwrap()
            .platforms
            .get_mut("linux-x86_64")
            .unwrap()
            .signature = "a different signature".to_owned();
        let err = manifest.validate().expect_err("should reject");
        assert!(
            err.to_string().contains("signature mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_a_delta_entry_with_no_fallback() {
        let mut manifest = manifest();
        manifest.platforms.clear();
        let err = manifest.validate().expect_err("should reject");
        assert!(
            err.to_string().contains("no matching full-download target"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_a_delta_entry_targeting_another_version() {
        let mut manifest = manifest();
        manifest.version = "2.0.0".to_owned();
        assert!(matches!(manifest.validate(), Err(Error::Manifest(_))));
    }

    #[test]
    fn rejects_malformed_digests() {
        let mut manifest = manifest();
        manifest
            .delta
            .as_mut()
            .unwrap()
            .platforms
            .get_mut("linux-x86_64")
            .unwrap()
            .target_installer_blake3 = "not a digest".to_owned();
        assert!(matches!(
            manifest.validate(),
            Err(Error::InvalidHash { .. })
        ));
    }
}
