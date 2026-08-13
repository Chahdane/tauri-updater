//! Pins the upstream versions this plugin's security claims were verified against.
//!
//! # Why a test rather than a sentence in the README
//!
//! Every security-relevant thing this plugin does rests on a fact about
//! `tauri-plugin-updater` that was established by reading its source:
//!
//! | Fact | Where it was read | What depends on it |
//! | --- | --- | --- |
//! | `Update::install` verifies nothing | `updater.rs:718` | The whole reason this plugin verifies at all (DECISIONS #10) |
//! | `verify_signature` is called from exactly one place | `updater.rs:712` | Same |
//! | The verifier's exact steps | `updater.rs:1453` | Our reproduction accepting precisely what Tauri accepts (#6) |
//! | `Update::raw_json` retains the fetched document | `updater.rs:492`, `:552` | The single-fetch identity (#13) |
//! | `get_urls` tries `{os}-{arch}-{installer}` first | `updater.rs:1420` | Binding delta metadata to Tauri's selection (#13) |
//! | `validate_endpoints`' http policy | `config.rs:145` | Mirroring it in `HttpFetch` (#19) |
//!
//! Not one of those is guaranteed by upstream's public API. They are
//! observations about an implementation, and an implementation is free to change
//! in a patch release. A `"2"` requirement would have let all six drift while
//! every test here stayed green — the tests would still pass, because they test
//! *our* reproduction of a behaviour that had moved.
//!
//! So the requirement in `Cargo.toml` is narrow, and this test asserts the
//! *resolved* version is one a human actually read. A dependency bump that steps
//! outside the verified set fails here, which is the point: the failure is a
//! prompt to re-read the source and extend the list, not a formality to bump.
//!
//! This is the same move as the determinism test pinning an exact BLAKE3 digest
//! rather than asserting "the patch is deterministic". A claim nothing can
//! falsify is not evidence.

use std::path::Path;

/// Versions of `tauri-plugin-updater` whose source has been read and whose
/// behaviour the table above was checked against.
///
/// Add to this list **only** after re-reading each of those six sites in the new
/// version. Widening it without doing that reading converts a verified claim
/// into an assumed one, silently.
const VERIFIED_UPDATER_VERSIONS: &[&str] = &["2.10.1"];

/// Read the resolved version of `name` from the workspace lock file.
fn locked_version(name: &str) -> Option<String> {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Cargo.lock")
        .canonicalize()
        .ok()?;
    let text = std::fs::read_to_string(lock).ok()?;

    // Cargo.lock is a sequence of `[[package]]` blocks; find the one whose name
    // matches and take the `version` line from that block.
    text.split("[[package]]")
        .find(|block| block.contains(&format!("name = \"{name}\"")))
        .and_then(|block| {
            block
                .lines()
                .find_map(|line| line.trim().strip_prefix("version = "))
        })
        .map(|v| v.trim().trim_matches('"').to_owned())
}

#[test]
fn the_resolved_updater_is_a_version_someone_actually_read() {
    let resolved = locked_version("tauri-plugin-updater")
        .expect("tauri-plugin-updater should be in the workspace lock file");

    assert!(
        VERIFIED_UPDATER_VERSIONS.contains(&resolved.as_str()),
        "tauri-plugin-updater resolved to {resolved}, which is not in the verified \
         set {VERIFIED_UPDATER_VERSIONS:?}.\n\n\
         This is not a formality. Six behaviours this plugin's security rests on \
         are observations about upstream's implementation, not guarantees of its \
         public API — see this file's header for the list and the line numbers. \
         Before adding {resolved} to VERIFIED_UPDATER_VERSIONS, re-read each one \
         in the new source and confirm it still holds."
    );
}

#[test]
fn the_version_requirement_cannot_silently_widen() {
    // Guards the other half: the lock could be right while the requirement in
    // Cargo.toml is broad enough to resolve elsewhere on someone else's machine,
    // or after a `cargo update`.
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read the plugin manifest");

    let requirement = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("tauri-plugin-updater = "))
        .expect("tauri-plugin-updater should be a declared dependency")
        .trim()
        .trim_matches('"')
        .to_owned();

    assert!(
        !requirement.is_empty() && requirement != "2" && requirement != "^2",
        "tauri-plugin-updater is required as {requirement:?}, which admits any 2.x. \
         The security claims were verified against specific versions, so the \
         requirement must be bounded — see docs/DECISIONS.md #21."
    );
    assert!(
        requirement.contains('<'),
        "the requirement {requirement:?} has no upper bound, so a future release \
         can be resolved without anyone having read it"
    );
}

#[test]
fn every_verified_version_is_valid_semver() {
    // Cheap guard against a typo turning the pin into something that can never
    // match, which would make the first test fail for the wrong reason.
    for version in VERIFIED_UPDATER_VERSIONS {
        semver::Version::parse(version)
            .unwrap_or_else(|e| panic!("{version:?} in the verified set is not semver: {e}"));
    }
}
