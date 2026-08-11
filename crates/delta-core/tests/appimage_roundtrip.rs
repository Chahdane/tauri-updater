//! The flagship test: prove a patch rebuilds an update artifact exactly.
//!
//! An AppImage is used because it is the first target platform and the simplest
//! artifact — one self-contained file, no installer step, no code signature to
//! preserve. Round-tripping one is pure file I/O, so this suite runs anywhere,
//! including on the macOS machine this is being developed on.
//!
//! The happy path is only half the job. A delta engine that silently produces a
//! *slightly wrong* artifact is far more dangerous than one that fails loudly,
//! so most of what follows is failure cases: corrupt patches, truncated patches,
//! and patches applied against the wrong base version. None of them may ever
//! yield a file that passes verification.

mod common;

use std::path::Path;

use tauri_updater_delta_core::backend::{backend_for, PatchBackend, ZstdBackend};
use tauri_updater_delta_core::hash::{verify_file, FileHash};
use tauri_updater_delta_core::Error;

/// Diffing at level 19 is deliberately slow; tests that only care about
/// correctness use a fast level instead.
const FAST: i32 = 3;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temp dir")
}

/// Assert that whatever `apply` did, the result is not a file that verifies as
/// the real artifact. Either an error or a mismatch is acceptable; silence is
/// not.
fn assert_not_mistaken_for(result: tauri_updater_delta_core::Result<()>, out: &Path, real: &Path) {
    if result.is_err() {
        return;
    }
    let expected = FileHash::of_file(real).expect("hash real artifact");
    assert!(
        verify_file(out, &expected).is_err(),
        "apply succeeded and the output verified as the real artifact, but the \
         inputs were wrong — a bad patch must never be mistaken for a good one"
    );
}

#[test]
fn rebuilds_the_new_appimage_exactly() {
    let dir = tempdir();
    let fixture = common::appimage_pair(dir.path());
    let patch = dir.path().join("1.0.0-to-1.0.1.patch");
    let rebuilt = dir.path().join("rebuilt.AppImage");

    let backend = ZstdBackend::new();
    backend
        .diff(&fixture.old, &fixture.new, &patch)
        .expect("diff should succeed");
    backend
        .apply(&fixture.old, &patch, &rebuilt)
        .expect("apply should succeed");

    // The assertion the whole project rests on.
    let expected = FileHash::of_file(&fixture.new).expect("hash released artifact");
    verify_file(&rebuilt, &expected).expect("rebuilt artifact must match the released one");

    let full = common::size_of(&fixture.new);
    let patch_size = common::size_of(&patch);
    println!(
        "artifact {full} bytes ({:.2} MiB) | patch {patch_size} bytes ({:.2} MiB) | \
         {:.2}% of a full download | {:.1}x smaller",
        full as f64 / 1_048_576.0,
        patch_size as f64 / 1_048_576.0,
        patch_size as f64 / full as f64 * 100.0,
        full as f64 / patch_size as f64,
    );
}

#[test]
fn patch_is_a_small_fraction_of_the_artifact() {
    let dir = tempdir();
    let fixture = common::appimage_pair(dir.path());
    let patch = dir.path().join("delta.patch");

    ZstdBackend::new()
        .diff(&fixture.old, &fixture.new, &patch)
        .expect("diff should succeed");

    let full = common::size_of(&fixture.new);
    let patch_size = common::size_of(&patch);

    // Roughly 5.7% of the fixture is genuinely new. Allowing 15% leaves room for
    // zstd version differences while still failing loudly if the patch stops
    // being a delta and turns into a re-compression of the whole artifact.
    assert!(
        patch_size * 100 < full * 15,
        "patch is {patch_size} bytes against a {full} byte artifact — not a delta"
    );
}

#[test]
fn identical_versions_produce_a_negligible_patch() {
    let dir = tempdir();
    let fixture = common::appimage_pair(dir.path());
    let patch = dir.path().join("noop.patch");
    let rebuilt = dir.path().join("rebuilt.AppImage");

    let backend = ZstdBackend::new().with_level(FAST);
    backend
        .diff(&fixture.old, &fixture.old, &patch)
        .expect("diff should succeed");
    backend
        .apply(&fixture.old, &patch, &rebuilt)
        .expect("apply should succeed");

    let expected = FileHash::of_file(&fixture.old).expect("hash artifact");
    verify_file(&rebuilt, &expected).expect("rebuilding an unchanged artifact must be exact");

    let patch_size = common::size_of(&patch);
    assert!(
        patch_size < 64 * 1024,
        "an unchanged artifact should compress to almost nothing, got {patch_size} bytes"
    );
}

#[test]
fn applying_against_the_wrong_base_never_verifies() {
    let dir = tempdir();
    let fixture = common::appimage_pair(dir.path());
    let patch = dir.path().join("delta.patch");
    let rebuilt = dir.path().join("rebuilt.AppImage");

    let backend = ZstdBackend::new().with_level(FAST);
    backend
        .diff(&fixture.old, &fixture.new, &patch)
        .expect("diff should succeed");

    // A user on some other version, or a corrupted cached artifact.
    let wrong_base = common::write(dir.path(), "wrong.AppImage", b"not the base artifact");
    let result = backend.apply(&wrong_base, &patch, &rebuilt);

    assert_not_mistaken_for(result, &rebuilt, &fixture.new);
}

#[test]
fn a_corrupted_patch_never_verifies() {
    let dir = tempdir();
    let fixture = common::appimage_pair(dir.path());
    let patch = dir.path().join("delta.patch");
    let rebuilt = dir.path().join("rebuilt.AppImage");

    let backend = ZstdBackend::new().with_level(FAST);
    backend
        .diff(&fixture.old, &fixture.new, &patch)
        .expect("diff should succeed");

    // Flip bits through the middle of the patch, as a damaged download would.
    let mut bytes = std::fs::read(&patch).expect("read patch");
    let start = bytes.len() / 3;
    for byte in &mut bytes[start..start + 64] {
        *byte ^= 0xFF;
    }
    let corrupt = common::write(dir.path(), "corrupt.patch", &bytes);

    let result = backend.apply(&fixture.old, &corrupt, &rebuilt);
    assert_not_mistaken_for(result, &rebuilt, &fixture.new);
}

#[test]
fn a_truncated_patch_is_reported_as_truncated() {
    let dir = tempdir();
    let fixture = common::appimage_pair(dir.path());
    let patch = dir.path().join("delta.patch");
    let rebuilt = dir.path().join("rebuilt.AppImage");

    let backend = ZstdBackend::new().with_level(FAST);
    backend
        .diff(&fixture.old, &fixture.new, &patch)
        .expect("diff should succeed");

    // An interrupted download: valid bytes, but the frame never ends.
    let bytes = std::fs::read(&patch).expect("read patch");
    let cut = common::write(dir.path(), "cut.patch", &bytes[..bytes.len() / 2]);

    let err = backend
        .apply(&fixture.old, &cut, &rebuilt)
        .expect_err("a truncated patch must not apply cleanly");
    assert!(
        matches!(err, Error::TruncatedPatch),
        "expected a truncation error, got: {err}"
    );
}

#[test]
fn refuses_to_write_past_the_output_ceiling() {
    let dir = tempdir();
    let fixture = common::appimage_pair(dir.path());
    let patch = dir.path().join("delta.patch");
    let rebuilt = dir.path().join("rebuilt.AppImage");

    ZstdBackend::new()
        .with_level(FAST)
        .diff(&fixture.old, &fixture.new, &patch)
        .expect("diff should succeed");

    // Stands in for a patch crafted to expand until the disk fills.
    let err = ZstdBackend::new()
        .with_max_output_bytes(1024)
        .apply(&fixture.old, &patch, &rebuilt)
        .expect_err("apply must stop at the ceiling");
    assert!(
        matches!(err, Error::OutputTooLarge { limit: 1024 }),
        "expected an output ceiling error, got: {err}"
    );
}

#[test]
fn round_trips_degenerate_artifacts() {
    let dir = tempdir();
    let backend = ZstdBackend::new().with_level(FAST);

    for (name, old, new) in [
        ("empty-to-empty", &b""[..], &b""[..]),
        ("empty-to-content", &b""[..], &b"a whole new artifact"[..]),
        ("content-to-empty", &b"the old artifact"[..], &b""[..]),
        ("single-byte", &b"a"[..], &b"b"[..]),
    ] {
        let old_path = common::write(dir.path(), &format!("{name}.old"), old);
        let new_path = common::write(dir.path(), &format!("{name}.new"), new);
        let patch = dir.path().join(format!("{name}.patch"));
        let rebuilt = dir.path().join(format!("{name}.rebuilt"));

        backend
            .diff(&old_path, &new_path, &patch)
            .unwrap_or_else(|e| panic!("{name}: diff failed: {e}"));
        backend
            .apply(&old_path, &patch, &rebuilt)
            .unwrap_or_else(|e| panic!("{name}: apply failed: {e}"));

        let expected = FileHash::of_bytes(new);
        verify_file(&rebuilt, &expected)
            .unwrap_or_else(|e| panic!("{name}: rebuilt artifact differs: {e}"));
    }
}

#[test]
fn a_backend_resolved_from_a_manifest_id_round_trips() {
    let dir = tempdir();
    let old = common::write(dir.path(), "old.bin", b"version one of the artifact");
    let new = common::write(dir.path(), "new.bin", b"version two of the artifact");
    let patch = dir.path().join("delta.patch");
    let rebuilt = dir.path().join("rebuilt.bin");

    // The path a client actually takes: read an id from the manifest, look up
    // the backend, apply, verify.
    let backend = backend_for("zstd").expect("zstd backend should be available");
    backend
        .diff(&old, &new, &patch)
        .expect("diff should succeed");
    backend
        .apply(&old, &patch, &rebuilt)
        .expect("apply should succeed");

    verify_file(
        &rebuilt,
        &FileHash::of_bytes(b"version two of the artifact"),
    )
    .expect("rebuilt artifact must match");
}
