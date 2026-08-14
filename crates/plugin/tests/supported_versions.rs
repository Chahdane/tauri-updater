//! The v0.1 compatibility contract, checked against the files that define it.
//!
//! # Why this is a test
//!
//! The README states a supported-version table. Every row of it is a fact that
//! lives somewhere else — a dependency constraint, a workspace field, a pinned
//! CI environment variable, a constant in the engine. A table maintained by hand
//! against four other files is a table that becomes wrong, and the failure mode
//! is the worst kind: a documented compatibility promise that quietly stops
//! matching what the code will actually accept.
//!
//! So the README is the statement and this is the check. If a constraint moves,
//! this fails until the table is updated to match, rather than the table
//! decaying silently until an adopter discovers it.
//!
//! Deliberately not covered here: what the plugin has been *demonstrated*
//! against. That is an evidence claim, not a constraint, and it belongs in
//! `research/FINDINGS.md` where the experiment records are.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(root().join(relative))
        .unwrap_or_else(|e| panic!("reading {relative}: {e}"))
}

/// Assert `needle` appears in `haystack`, naming both when it does not.
fn contains(what: &str, haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "{what}: expected to find {needle:?}.\n\
         The README's supported-version table and this file must agree with the \
         constraint that actually applies."
    );
}

#[test]
fn the_readme_states_the_updater_range_the_plugin_actually_requires() {
    let manifest = read("crates/plugin/Cargo.toml");
    contains(
        "plugin Cargo.toml",
        &manifest,
        r#"tauri-plugin-updater = ">=2.10.1, <2.11.0""#,
    );
    let readme = read("README.md");
    contains("README", &readme, ">=2.10.1, <2.11.0");
}

#[test]
fn the_readme_states_the_declared_minimum_rust_version() {
    let workspace = read("Cargo.toml");
    let msrv = workspace
        .lines()
        .find_map(|line| line.strip_prefix("rust-version = "))
        .expect("workspace rust-version")
        .trim()
        .trim_matches('"')
        .to_owned();
    assert_eq!(
        msrv, "1.88",
        "if the MSRV moves, the README must move with it"
    );

    let readme = read("README.md");
    contains("README", &readme, &format!("Rust {msrv}"));
}

#[test]
fn the_readme_states_the_pinned_release_bundler() {
    // The recompression recipe was read out of the bundler this CLI version
    // ships. A release built with another one may produce an archive no client
    // can reproduce, so the pin is load-bearing rather than tidy.
    let workflow = read(".github/workflows/release.yml");
    contains(
        "release workflow",
        &workflow,
        r#"TAURI_CLI_VERSION: "2.10.1""#,
    );
    contains(
        "release workflow",
        &workflow,
        r#"--version "=${TAURI_CLI_VERSION}""#,
    );

    let readme = read("README.md");
    contains("README", &readme, "tauri-cli 2.10.1");
}

#[test]
fn the_readme_states_the_representation_and_recipe_identifiers() {
    // These two strings are the compatibility handshake between a release and a
    // client: a client that does not implement them declines the tar layer
    // rather than guessing.
    let readme = read("README.md");
    contains("README", &readme, "app-tar-gz-v1");
    contains("README", &readme, "tauri-app-tar-gz-v1");

    assert_eq!(
        tauri_updater_delta_core::manifest::REPRESENTATION_APP_TAR_GZ_V1,
        "app-tar-gz-v1"
    );
    assert_eq!(
        tauri_updater_delta_core::manifest::RECOMPRESSION_TAURI_APP_TAR_GZ_V1,
        "tauri-app-tar-gz-v1"
    );
}

#[test]
fn the_readme_states_the_delta_backend() {
    let readme = read("README.md");
    contains("README", &readme, "zstd");
}

#[test]
fn the_readme_does_not_claim_client_support_for_linux_or_windows() {
    // The engine is tested on all three platforms and the application path is
    // demonstrated on exactly one. Those are different claims and the README
    // must not let the first imply the second.
    let readme = read("README.md").to_lowercase();
    for forbidden in [
        "supports linux",
        "supports windows",
        "linux support",
        "windows support",
        "production ready",
        "production-ready",
    ] {
        assert!(
            !readme.contains(forbidden),
            "the README must not claim {forbidden:?} for v0.1"
        );
    }
}

#[test]
fn the_readme_does_not_claim_a_universal_size_reduction() {
    // "85% smaller" is the claim this project must never make: the measured
    // ratio belongs to two controlled example-app pairs, and a real application
    // with a different change shape will see something else entirely.
    let readme = read("README.md").to_lowercase();
    for forbidden in ["85% smaller", "updates are 85%", "always 15%", "up to 85%"] {
        assert!(
            !readme.contains(forbidden),
            "the README must not state {forbidden:?} as a general result"
        );
    }
    // And where a number does appear, its scope must appear with it.
    if readme.contains("15%") {
        assert!(
            readme.contains("controlled"),
            "a measured ratio must be qualified as a controlled measurement"
        );
    }
}
