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
        "--from-version",
        "--previous-installer",
        "--new-installer",
        "--installer-url",
        "--patch-url",
        "--patch-out",
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
