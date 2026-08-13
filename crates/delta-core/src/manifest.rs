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
//! # The optional tar layer
//!
//! A platform entry may additionally carry a [`tar_layer`](DeltaPlatform::tar_layer)
//! describing patches against the *uncompressed* contents of a compressed
//! installer. It is purely additive: `patches` keeps exactly the meaning it had,
//! the compressed target's digest, size and signature stay on the platform entry
//! and are **not** duplicated, and a client that does not implement the declared
//! representation or recompression recipe ignores the block.
//!
//! ```json
//! "tar_layer": {
//!   "representation": "app-tar-gz-v1",
//!   "recompression": "tauri-app-tar-gz-v1",
//!   "target_tar_blake3": "…",
//!   "target_tar_size": 9461248,
//!   "patches": {
//!     "1.0.1": {
//!       "backend_id": "zstd",
//!       "patch_url": "https://releases.example.com/1.0.1-to-1.0.2.tar.zst",
//!       "patch_blake3": "…",
//!       "patch_size": 625269,
//!       "base_installer_blake3": "…",
//!       "base_installer_size": 4070756,
//!       "base_tar_blake3": "…",
//!       "base_tar_size": 9461248
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

/// The only artifact representation the tar layer currently describes:
/// a macOS `.app.tar.gz`, patched at the uncompressed tar inside it.
pub const REPRESENTATION_APP_TAR_GZ_V1: &str = "app-tar-gz-v1";

/// The only recompression recipe this build implements.
///
/// Names the *whole* recipe, not just the compressor: `tar::Builder`'s write
/// topology replayed into `flate2::write::GzEncoder` at `Compression::default()`
/// over a flate2 build whose backend is zlib-rs. See [`crate::recompress`] for
/// what is pinned and what is merely observed.
pub const RECOMPRESSION_TAURI_APP_TAR_GZ_V1: &str = "tauri-app-tar-gz-v1";

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
    ///
    /// These patch the **compressed installer directly**, and their meaning is
    /// unchanged by the addition of [`tar_layer`](DeltaPlatform::tar_layer).
    pub patches: BTreeMap<String, Patch>,

    /// Optional patches against the *uncompressed* representation inside the
    /// installer, for artifacts where that is dramatically smaller.
    ///
    /// Absent means the tar path is not published for this platform, which is
    /// ordinary — every reader falls back to `patches`, then to a full download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tar_layer: Option<TarLayer>,
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

/// Tar-layer patching for one platform.
///
/// # Why this is not a schema bump
///
/// A `.app.tar.gz` is a gzip stream, and gzip output shifts wholesale when the
/// input changes, so a patch between two compressed artifacts saves almost
/// nothing (`docs/DECISIONS.md` #15). Patching the tar *inside* it saves a great
/// deal — but only for a client that can rebuild the exact compressed artifact
/// afterwards, because the signature covers those exact bytes (#6).
///
/// That capability is per-client, not per-release. A client that has it uses
/// this block; one that does not ignores it and reads
/// [`patches`](DeltaPlatform::patches) exactly as before. Bumping
/// [`SCHEMA_VERSION`] would instead make every existing client refuse the whole
/// document, which is the opposite of what an additive optimisation should do.
///
/// # Version-tagged, not feature-detected
///
/// [`representation`](TarLayer::representation) names what the compressed
/// artifact contains, and [`recompression`](TarLayer::recompression) names the
/// recipe for putting it back. Both are opaque identifiers a client either
/// implements or does not. **Changing what an existing identifier means is
/// forbidden**; incompatible semantics get a new identifier, so an older client
/// declines cleanly instead of producing bytes it believes are exact and is
/// wrong about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TarLayer {
    /// What the compressed installer contains, e.g. [`REPRESENTATION_APP_TAR_GZ_V1`].
    ///
    /// Unknown values mean the tar path is unsupported by this client, which is
    /// a fallback and never an error.
    pub representation: String,

    /// How to rebuild the exact compressed installer from the exact tar, e.g.
    /// [`RECOMPRESSION_TAURI_APP_TAR_GZ_V1`].
    pub recompression: String,

    /// Digest of the exact tar a tar patch must reproduce.
    ///
    /// Distinct from [`DeltaPlatform::target_installer_blake3`], which covers the
    /// *compressed* artifact. Both are checked: the tar gate proves the patch
    /// applied, the installer gate proves the recompression recipe reproduced
    /// the published bytes.
    pub target_tar_blake3: String,

    /// Size of that tar in bytes. Bounds decompression and the patch backend's
    /// window before either runs.
    pub target_tar_size: u64,

    /// Tar patches into this release, keyed by the version upgraded *from*.
    pub patches: BTreeMap<String, TarPatch>,
}

/// One tar-layer patch, from a previous version to the release.
///
/// Carries the *base* side's properties, which the direct-patch [`Patch`] has no
/// need for: the tar path starts from a locally cached artifact rather than from
/// a file the server just sent, so the client must be able to decide whether
/// what it has is the right base before spending anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TarPatch {
    /// Backend that produced it, e.g. `"zstd"`.
    pub backend_id: String,
    /// Where the patch can be downloaded.
    pub patch_url: String,
    /// Digest of the patch file itself.
    pub patch_blake3: String,
    /// Size of the patch in bytes.
    pub patch_size: u64,

    /// Digest of the **compressed** installer this patch expects as its base.
    ///
    /// Lets a client check its cache against the release's idea of the base
    /// before decompressing anything.
    pub base_installer_blake3: String,
    /// Size of that compressed installer.
    pub base_installer_size: u64,
    /// Digest of the exact tar inside it — what the patch actually applies to.
    pub base_tar_blake3: String,
    /// Size of that tar. Bounds decompression of the cached artifact.
    pub base_tar_size: u64,
}

/// Whether this build can take a platform's tar path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TarSupport {
    /// Both identifiers are implemented here.
    Supported,
    /// The representation is one this build does not know how to read.
    UnknownRepresentation(String),
    /// The recompression recipe is one this build cannot perform.
    UnknownRecompression(String),
}

impl TarLayer {
    /// Whether this build implements both declared identifiers.
    ///
    /// Deliberately an exact-match allow-list rather than a prefix or version
    /// comparison. An identifier is a name, not an ordering: "later" tells you
    /// nothing about whether the bytes you would produce are the published ones.
    pub fn support(&self) -> TarSupport {
        if self.representation != REPRESENTATION_APP_TAR_GZ_V1 {
            return TarSupport::UnknownRepresentation(self.representation.clone());
        }
        if self.recompression != RECOMPRESSION_TAURI_APP_TAR_GZ_V1 {
            return TarSupport::UnknownRecompression(self.recompression.clone());
        }
        TarSupport::Supported
    }

    /// Whether [`support`](TarLayer::support) is [`TarSupport::Supported`].
    pub fn is_supported(&self) -> bool {
        self.support() == TarSupport::Supported
    }
}

impl DeltaPlatform {
    /// The tar-layer patch from `from_version`, if this build can use it.
    ///
    /// `Err` carries why the tar path is unavailable, for logging. Both arms are
    /// ordinary: the caller tries the direct patch next, then a full download.
    pub fn tar_patch(
        &self,
        from_version: &str,
    ) -> std::result::Result<Option<(&TarLayer, &TarPatch)>, TarSupport> {
        let Some(layer) = &self.tar_layer else {
            return Ok(None);
        };
        match layer.support() {
            TarSupport::Supported => {
                Ok(layer.patches.get(from_version).map(|patch| (layer, patch)))
            }
            unsupported => Err(unsupported),
        }
    }
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

            if let Some(tar) = &entry.tar_layer {
                validate_tar_layer(platform, &entry.target_version, tar)?;
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

/// Structural checks for a tar layer.
///
/// # What is deliberately *not* checked here
///
/// Unknown [`representation`](TarLayer::representation) and
/// [`recompression`](TarLayer::recompression) values are **valid**. Rejecting
/// them would make a document published for a newer client unreadable by an
/// older one — and since `plan_update` parses the whole manifest in one go, that
/// would take the platform's *direct* patches down with the tar layer it does not
/// understand. Support is a client capability, so it is decided at selection time
/// by [`TarLayer::support`], not at parse time.
///
/// Everything below is different in kind: a well-formed document cannot contain
/// it whatever the reader's capabilities, so failing here is right.
fn validate_tar_layer(platform: &str, target_version: &str, tar: &TarLayer) -> Result<()> {
    if tar.representation.is_empty() || tar.recompression.is_empty() {
        return Err(Error::Manifest(format!(
            "tar layer for {platform} must name both a representation and a recompression recipe"
        )));
    }

    FileHash::from_hex(&tar.target_tar_blake3)?;
    if tar.target_tar_size == 0 {
        return Err(Error::Manifest(format!(
            "tar layer for {platform} declares a zero-byte target tar"
        )));
    }

    for (from, patch) in &tar.patches {
        if from == target_version {
            return Err(Error::Manifest(format!(
                "tar layer for {platform} patches {from} to itself"
            )));
        }
        FileHash::from_hex(&patch.patch_blake3)?;
        FileHash::from_hex(&patch.base_installer_blake3)?;
        FileHash::from_hex(&patch.base_tar_blake3)?;
        if patch.base_tar_size == 0 || patch.base_installer_size == 0 {
            return Err(Error::Manifest(format!(
                "tar patch {from} for {platform} declares a zero-byte base"
            )));
        }
        // A base that is bit-identical to the target is not a patchable pair; it
        // means the release process patched a version against itself under a
        // different name, and the client would install what it already has.
        if patch.base_tar_blake3 == tar.target_tar_blake3 {
            return Err(Error::Manifest(format!(
                "tar patch {from} for {platform} declares the target tar as its own base"
            )));
        }
    }

    Ok(())
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
                        tar_layer: None,
                    },
                )]),
            }),
        }
    }

    fn tar_patch() -> TarPatch {
        TarPatch {
            backend_id: "zstd".to_owned(),
            patch_url: "https://example.com/1.0.0-to-1.0.1.tar.zst".to_owned(),
            patch_blake3: FileHash::of_bytes(b"tar patch").to_hex(),
            patch_size: 625_269,
            base_installer_blake3: FileHash::of_bytes(b"base installer").to_hex(),
            base_installer_size: 4_070_756,
            base_tar_blake3: FileHash::of_bytes(b"base tar").to_hex(),
            base_tar_size: 9_461_248,
        }
    }

    fn tar_layer() -> TarLayer {
        TarLayer {
            representation: REPRESENTATION_APP_TAR_GZ_V1.to_owned(),
            recompression: RECOMPRESSION_TAURI_APP_TAR_GZ_V1.to_owned(),
            target_tar_blake3: FileHash::of_bytes(b"target tar").to_hex(),
            target_tar_size: 9_461_248,
            patches: BTreeMap::from([("1.0.0".to_owned(), tar_patch())]),
        }
    }

    /// The fixture manifest with a tar layer bolted onto its one platform.
    fn with_tar_layer(mutate: impl FnOnce(&mut TarLayer)) -> Manifest {
        let mut manifest = manifest();
        let mut layer = tar_layer();
        mutate(&mut layer);
        manifest
            .delta
            .as_mut()
            .unwrap()
            .platforms
            .get_mut("linux-x86_64")
            .unwrap()
            .tar_layer = Some(layer);
        manifest
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

    // ---- the tar layer --------------------------------------------------

    #[test]
    fn a_tar_layer_round_trips_and_stays_optional() {
        let with = with_tar_layer(|_| {});
        with.validate().expect("a tar layer is valid");
        let parsed = Manifest::from_json(&with.to_json().expect("serialize")).expect("parse");
        assert_eq!(with, parsed);

        // And the field is genuinely absent when unused, rather than serialised
        // as null — a delta-aware client on the old schema must see no change.
        let without = manifest().to_json().expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&without).expect("json");
        assert!(
            value["delta"]["platforms"]["linux-x86_64"]
                .get("tar_layer")
                .is_none(),
            "an unused tar layer must not appear in the document at all"
        );
    }

    #[test]
    fn a_document_written_before_the_tar_layer_still_parses() {
        // The backwards-compatibility claim, made against a literal document
        // rather than against a struct this build happens to be able to
        // construct. Nothing here mentions the tar layer.
        let json = r#"{
            "version": "1.0.1",
            "platforms": {
                "linux-x86_64": { "url": "https://example.com/a", "signature": "sig" }
            },
            "delta": {
                "schema": 1,
                "hash_algo": "blake3",
                "platforms": {
                    "linux-x86_64": {
                        "target_version": "1.0.1",
                        "target_installer_blake3": "0000000000000000000000000000000000000000000000000000000000000000",
                        "target_installer_size": 100,
                        "signature": "sig",
                        "patches": {
                            "1.0.0": {
                                "backend_id": "zstd",
                                "patch_url": "https://example.com/p",
                                "patch_blake3": "1111111111111111111111111111111111111111111111111111111111111111",
                                "patch_size": 10
                            }
                        }
                    }
                }
            }
        }"#;
        let manifest =
            Manifest::from_json(json).expect("a pre-tar-layer document must still parse");
        let (entry, patch) = manifest
            .patch_for("linux-x86_64", "1.0.0")
            .expect("the direct patch must still be found");
        assert!(entry.tar_layer.is_none());
        assert_eq!(patch.patch_size, 10);
        assert_eq!(
            entry.tar_patch("1.0.0"),
            Ok(None),
            "no tar layer is ordinary, not unsupported"
        );
    }

    #[test]
    fn the_approved_identifiers_are_the_supported_ones() {
        assert_eq!(tar_layer().support(), TarSupport::Supported);
        assert_eq!(REPRESENTATION_APP_TAR_GZ_V1, "app-tar-gz-v1");
        assert_eq!(RECOMPRESSION_TAURI_APP_TAR_GZ_V1, "tauri-app-tar-gz-v1");
    }

    #[test]
    fn an_unknown_representation_is_valid_but_unsupported() {
        // Both halves matter. Valid, because a document published for a newer
        // client must stay readable — including its *direct* patches, which this
        // build can still use. Unsupported, because producing bytes from a
        // representation we do not implement is exactly what must not happen.
        let manifest = with_tar_layer(|l| l.representation = "app-tar-zstd-v2".to_owned());
        manifest
            .validate()
            .expect("an unknown representation must not invalidate the document");

        let entry = &manifest.delta.as_ref().unwrap().platforms["linux-x86_64"];
        assert_eq!(
            entry.tar_patch("1.0.0"),
            Err(TarSupport::UnknownRepresentation("app-tar-zstd-v2".into()))
        );
        assert!(
            entry.patches.contains_key("1.0.0"),
            "the direct patch must survive a tar layer we cannot read"
        );
    }

    #[test]
    fn an_unknown_recompression_recipe_is_valid_but_unsupported() {
        let manifest = with_tar_layer(|l| l.recompression = "tauri-app-tar-gz-v9".to_owned());
        manifest.validate().expect("still a valid document");
        let entry = &manifest.delta.as_ref().unwrap().platforms["linux-x86_64"];
        assert_eq!(
            entry.tar_patch("1.0.0"),
            Err(TarSupport::UnknownRecompression(
                "tauri-app-tar-gz-v9".into()
            ))
        );
    }

    #[test]
    fn a_supported_layer_with_no_patch_from_this_version_is_ordinary() {
        let manifest = with_tar_layer(|_| {});
        let entry = &manifest.delta.as_ref().unwrap().platforms["linux-x86_64"];
        assert_eq!(entry.tar_patch("0.9.0"), Ok(None));
        assert!(entry.tar_patch("1.0.0").expect("supported").is_some());
    }

    #[test]
    fn rejects_a_tar_layer_with_an_empty_identifier() {
        for manifest in [
            with_tar_layer(|l| l.representation = String::new()),
            with_tar_layer(|l| l.recompression = String::new()),
        ] {
            let err = manifest.validate().expect_err("should reject");
            assert!(
                err.to_string().contains("must name both"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn rejects_malformed_tar_digests() {
        // Every digest field, one at a time: a bad digest must be caught at
        // parse time, not after a cached artifact has been decompressed.
        let cases: Vec<Manifest> = vec![
            with_tar_layer(|l| l.target_tar_blake3 = "nope".to_owned()),
            with_tar_layer(|l| {
                l.patches.get_mut("1.0.0").unwrap().patch_blake3 = "nope".to_owned()
            }),
            with_tar_layer(|l| {
                l.patches.get_mut("1.0.0").unwrap().base_installer_blake3 = "nope".to_owned()
            }),
            with_tar_layer(|l| {
                l.patches.get_mut("1.0.0").unwrap().base_tar_blake3 = "nope".to_owned()
            }),
        ];
        for (i, manifest) in cases.iter().enumerate() {
            assert!(
                matches!(manifest.validate(), Err(Error::InvalidHash { .. })),
                "case {i} should have been rejected"
            );
        }
    }

    #[test]
    fn rejects_zero_sized_tar_declarations() {
        for manifest in [
            with_tar_layer(|l| l.target_tar_size = 0),
            with_tar_layer(|l| l.patches.get_mut("1.0.0").unwrap().base_tar_size = 0),
            with_tar_layer(|l| l.patches.get_mut("1.0.0").unwrap().base_installer_size = 0),
        ] {
            assert!(
                matches!(manifest.validate(), Err(Error::Manifest(_))),
                "a zero-byte declaration must be rejected"
            );
        }
    }

    #[test]
    fn rejects_a_tar_patch_from_the_release_to_itself() {
        let manifest = with_tar_layer(|l| {
            let patch = l.patches.remove("1.0.0").unwrap();
            l.patches.insert("1.0.1".to_owned(), patch);
        });
        let err = manifest.validate().expect_err("should reject");
        assert!(
            err.to_string().contains("to itself"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_a_tar_patch_whose_base_is_the_target() {
        // Distinct from the version check above: the versions differ, but the
        // declared bases are the same bytes, so applying the patch would install
        // what the user already has.
        let manifest = with_tar_layer(|l| {
            let target = l.target_tar_blake3.clone();
            l.patches.get_mut("1.0.0").unwrap().base_tar_blake3 = target;
        });
        let err = manifest.validate().expect_err("should reject");
        assert!(
            err.to_string().contains("its own base"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn the_compressed_target_is_described_once() {
        // The no-duplication rule, asserted rather than assumed: the tar layer
        // must not carry its own copy of the compressed installer's digest,
        // size or signature. Two copies of one fact is a way for them to
        // disagree, which is the whole reason DECISIONS #13 exists.
        let json = with_tar_layer(|_| {}).to_json().expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        let layer = &value["delta"]["platforms"]["linux-x86_64"]["tar_layer"];
        for forbidden in [
            "target_installer_blake3",
            "target_installer_size",
            "signature",
            "target_version",
        ] {
            assert!(
                layer.get(forbidden).is_none(),
                "{forbidden} belongs to the platform entry, not the tar layer"
            );
        }
    }
}
