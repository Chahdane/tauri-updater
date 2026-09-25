//! Tests that run the actual `delta-release` executable.
//!
//! # Why this file exists
//!
//! The crate had a bug that made **every invocation of a debug build panic**,
//! including `--help`, and it survived a full test suite plus a CI step that ran
//! the binary. Both misses are instructive, and this file is the fix for both.
//!
//! The bug: the application declared `--version` for the release being built,
//! while `#[command(version)]` declares clap's own `--version`. Clap detects the
//! duplicate in a `debug_assert`, so a debug build aborts with
//!
//! ```text
//! Command delta-release: Argument names must be unique, but 'version' is in
//! use by more than one argument or group
//! ```
//!
//! **Why the library tests missed it:** they all call `build_release` and the
//! other library entry points directly. Argument parsing is not library code, so
//! nothing exercised it. A test suite can be thorough about the logic and still
//! never touch the boundary a user actually arrives at.
//!
//! **Why the CI smoke check missed it:** `release.yml` ran
//! `cargo run --release … -- --help`. Clap's uniqueness check is a *debug*
//! assertion, compiled out under `--release`, so the one step that did invoke
//! the binary was run in the single configuration where the bug is invisible.
//!
//! So the rule these tests encode: **run the real executable, in the profile
//! that checks the most.** These are ordinary `cargo test` targets, so they run
//! in debug by default, which is exactly where the assertion lives.

use std::process::Command;

/// The binary cargo just built for this test.
///
/// `CARGO_BIN_EXE_*` is set by cargo for integration tests, so this is the real
/// executable rather than a library call pretending to be one.
fn delta_release() -> Command {
    Command::new(env!("CARGO_BIN_EXE_delta-release"))
}

#[test]
fn the_binary_runs_at_all() {
    // The regression test in its purest form. Before the rename this exited 101
    // with a clap panic, for *any* arguments at all.
    let out = delta_release().arg("--help").output().expect("run --help");

    assert!(
        out.status.success(),
        "--help exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("panicked"),
        "the binary panicked: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn version_reports_the_tool_not_the_release() {
    // The collision resolved in the direction that matters: `--version` is
    // clap's, reporting the tool. An operator asking a CLI its version and
    // getting a parse error instead is the smaller half of the bug, but it is
    // still a bug.
    let out = delta_release()
        .arg("--version")
        .output()
        .expect("run --version");

    assert!(out.status.success(), "--version should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("delta-release") && stdout.contains(env!("CARGO_PKG_VERSION")),
        "expected the tool's own version, got: {stdout}"
    );
}

#[test]
fn the_release_version_is_target_version() {
    // The replacement flag is present and takes a value.
    let out = delta_release().arg("--help").output().expect("run --help");
    let help = String::from_utf8_lossy(&out.stdout);

    assert!(
        help.contains("--target-version"),
        "the release version flag should be --target-version:\n{help}"
    );
    assert!(
        !help.contains("--version <"),
        "--version must not take a value; it is clap's flag now:\n{help}"
    );
}

#[test]
fn missing_required_arguments_fail_cleanly() {
    // A usage error, not a panic. Distinguishing the two is the whole point:
    // exit code 2 with a usage message is clap working, 101 is clap aborting.
    let out = delta_release()
        .arg("--target-version")
        .arg("1.0.1")
        .output()
        .expect("run with incomplete args");

    assert!(!out.status.success(), "incomplete arguments should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "a usage error must not be a panic: {stderr}"
    );
    assert!(
        stderr.contains("required"),
        "the error should say what is missing: {stderr}"
    );
}

#[test]
fn every_documented_flag_is_accepted() {
    // Guards the whole surface the release workflow depends on. A rename that
    // fixed --version while breaking --patch-url would trade one broken release
    // for another, and only a test that names each flag catches that.
    let out = delta_release().arg("--help").output().expect("run --help");
    let help = String::from_utf8_lossy(&out.stdout);

    for flag in [
        "--platform",
        "--target-version",
        "--app-id",
        "--app-config",
        "--from-version",
        "--previous-installer",
        "--new-installer",
        "--installer-url",
        "--patch-url",
        "--patch-out",
        "--max-direct-patch-percent",
        "--require-direct-patch",
        "--manifest",
        "--notes",
        "--pub-date",
        "--private-key",
        "--dry-run",
        "--tar-patch-out",
        "--tar-patch-url",
        "--require-tar-layer",
        "--max-tar-bytes",
    ] {
        assert!(help.contains(flag), "{flag} missing from --help:\n{help}");
    }
}

#[test]
fn the_tar_layer_flags_are_useless_without_each_other() {
    // A patch generated with no URL to serve it from, or a URL with no patch
    // behind it, publishes a manifest entry that cannot work. clap can refuse
    // both at parse time, so it should.
    for args in [
        vec!["--tar-patch-out", "patch.zst"],
        vec!["--tar-patch-url", "https://example.com/p.zst"],
        vec!["--require-tar-layer"],
    ] {
        let out = delta_release()
            .args(&args)
            .output()
            .expect("run with a partial tar-layer flag set");
        assert!(
            !out.status.success(),
            "{args:?} should have been refused as incomplete"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("panicked"),
            "a usage error must not be a panic: {stderr}"
        );
    }
}

#[test]
fn repeated_predecessor_flags_must_have_matching_counts() {
    let out = delta_release()
        .env_remove("TAURI_SIGNING_PRIVATE_KEY")
        .args([
            "--platform",
            "windows-x86_64",
            "--target-version",
            "1.0.2",
            "--app-id",
            "dev.example.testapp",
            "--new-installer",
            "new.exe",
            "--installer-url",
            "https://example.com/new.exe",
            "--from-version",
            "1.0.1",
            "--from-version",
            "1.0.0",
            "--previous-installer",
            "one.exe",
            "--patch-url",
            "https://example.com/one.zst",
            "--patch-out",
            "one.zst",
        ])
        .output()
        .expect("run with mismatched repeated arguments");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("received 2 --from-version value(s), but 1 --previous-installer"),
        "the error must identify the mismatched group: {stderr}"
    );
}

#[test]
fn repeated_tar_layer_flags_must_match_the_predecessor_count() {
    let out = delta_release()
        .env_remove("TAURI_SIGNING_PRIVATE_KEY")
        .args([
            "--platform",
            "darwin-aarch64",
            "--target-version",
            "1.0.2",
            "--app-id",
            "dev.example.testapp",
            "--new-installer",
            "new.app.tar.gz",
            "--installer-url",
            "https://example.com/new.app.tar.gz",
            "--from-version",
            "1.0.1",
            "--previous-installer",
            "one.app.tar.gz",
            "--patch-url",
            "https://example.com/one.zst",
            "--patch-out",
            "one.zst",
            "--from-version",
            "1.0.0",
            "--previous-installer",
            "two.app.tar.gz",
            "--patch-url",
            "https://example.com/two.zst",
            "--patch-out",
            "two.zst",
            "--tar-patch-url",
            "https://example.com/one.tar.zst",
            "--tar-patch-out",
            "one.tar.zst",
        ])
        .output()
        .expect("run with a partial repeated tar-layer group");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("received 2 predecessor(s), but 1 tar-layer patch pair(s)"),
        "the error must identify the incomplete tar-layer set: {stderr}"
    );
}

#[test]
fn repeated_predecessor_groups_reach_release_processing() {
    let out = delta_release()
        .env_remove("TAURI_SIGNING_PRIVATE_KEY")
        .args([
            "--platform",
            "windows-x86_64",
            "--target-version",
            "1.0.2",
            "--app-id",
            "dev.example.testapp",
            "--new-installer",
            "new.exe",
            "--installer-url",
            "https://example.com/new.exe",
            "--from-version",
            "1.0.1",
            "--previous-installer",
            "one.exe",
            "--patch-url",
            "https://example.com/one.zst",
            "--patch-out",
            "one.zst",
            "--from-version",
            "1.0.0",
            "--previous-installer",
            "two.exe",
            "--patch-url",
            "https://example.com/two.zst",
            "--patch-out",
            "two.zst",
        ])
        .output()
        .expect("run with two complete predecessor groups");

    assert!(!out.status.success(), "the absent key should stop the run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no signing key"),
        "complete repeated groups should pass argument validation: {stderr}"
    );
}

#[test]
fn an_unknown_flag_is_rejected_rather_than_ignored() {
    // A release tool that silently ignores a misspelled flag publishes a
    // manifest describing something other than what was asked for.
    let out = delta_release()
        .arg("--not-a-real-flag")
        .output()
        .expect("run with a bad flag");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("unrecognized"),
        "expected an unknown-flag error: {stderr}"
    );
}

// ---- the release-version contract, through the real binaries --------------
//
// Audit finding A-2. The workflow derived `--target-version` from the git tag
// and never compared it against the version compiled into the application, so
// a crate-release tag would have signed the demonstration app under a version
// the app contradicts the moment it launches. The library tests in
// `version_contract` cover the rule; these cover the two command lines an
// operator actually types, because that is the boundary the release runs
// through.

/// The `release-check` binary cargo just built.
fn release_check() -> Command {
    Command::new(env!("CARGO_BIN_EXE_release-check"))
}

/// Write a minimal application whose two files agree on `version`.
fn write_app(dir: &std::path::Path, version: &str) -> std::path::PathBuf {
    let conf = dir.join("tauri.conf.json");
    std::fs::write(
        &conf,
        format!(
            r#"{{"productName":"Demo","version":"{version}","identifier":"dev.example.demo",
                 "plugins":{{"updater":{{"pubkey":"a-key"}}}}}}"#
        ),
    )
    .expect("write tauri.conf.json");
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]
name = \"demo\"
version = \"{version}\"
"
        ),
    )
    .expect("write Cargo.toml");
    conf
}

#[test]
fn the_generator_refuses_a_tag_that_does_not_name_the_built_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    let conf = write_app(dir.path(), "1.0.0");

    // No signing key is set, and that must not be what fails: the version
    // contract is checked first precisely so the useful error is the one an
    // operator sees.
    let out = delta_release()
        .args([
            "--platform",
            "darwin-aarch64",
            "--target-version",
            "0.1.0",
            "--app-config",
        ])
        .arg(&conf)
        .args(["--new-installer", "does-not-matter.bin"])
        .args(["--installer-url", "https://example.com/app.bin"])
        .env_remove("TAURI_SIGNING_PRIVATE_KEY")
        .output()
        .expect("run the generator");

    assert!(
        !out.status.success(),
        "a mismatched version must be refused"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("names version 0.1.0") && stderr.contains("is version 1.0.0"),
        "expected the version-contract refusal, got: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn the_generator_accepts_the_matching_tag_and_moves_on() {
    // The same invocation with the versions in step must get past the contract.
    // It still fails -- there is no key and no installer -- and what matters is
    // that it fails for one of those reasons rather than the version.
    let dir = tempfile::tempdir().expect("temp dir");
    let conf = write_app(dir.path(), "1.0.0");

    let out = delta_release()
        .args(["--platform", "darwin-aarch64", "--target-version", "1.0.0"])
        .arg("--app-config")
        .arg(&conf)
        .args(["--new-installer", "does-not-matter.bin"])
        .args(["--installer-url", "https://example.com/app.bin"])
        .env_remove("TAURI_SIGNING_PRIVATE_KEY")
        .output()
        .expect("run the generator");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("names version"),
        "the version contract should have passed, got: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn the_checker_refuses_a_tag_that_does_not_name_the_built_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    let conf = write_app(dir.path(), "1.0.0");
    let manifest = dir.path().join("manifest.json");
    std::fs::write(&manifest, r#"{"version":"0.1.0","platforms":{}}"#).expect("write manifest");

    let out = release_check()
        .arg("--manifest")
        .arg(&manifest)
        .args(["--tag", "v0.1.0"])
        .args(["--platform", "darwin-aarch64"])
        .arg("--artifact")
        .arg(dir.path().join("missing.bin"))
        .arg("--app-config")
        .arg(&conf)
        .output()
        .expect("run the checker");

    assert!(
        !out.status.success(),
        "a mismatched version must be refused"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("names version 0.1.0"),
        "expected the version-contract refusal, got: {stderr}"
    );
}

#[test]
fn the_checker_still_requires_an_app_id_and_key_without_a_config() {
    let out = release_check()
        .args(["--manifest", "manifest.json"])
        .args(["--tag", "v1.0.0"])
        .args(["--platform", "darwin-aarch64"])
        .args(["--artifact", "app.bin"])
        .output()
        .expect("run the checker");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("required"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
