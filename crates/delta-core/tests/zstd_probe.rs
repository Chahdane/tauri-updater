//! Research probe, not a regression test: how large is a zstd patch between two
//! large installer-like files under different compressor settings?
//!
//! Run with `DELTA_PROBE_OLD=<file> DELTA_PROBE_NEW=<file> cargo test -p
//! tauri-updater-delta-core --test zstd_probe --release -- --ignored --nocapture`.

use std::time::Instant;

use tauri_updater_delta_core::backend::{PatchBackend, ZstdBackend};
use zstd::zstd_safe::{self, CParameter};

fn window_log(size: usize) -> u32 {
    (usize::BITS - size.saturating_sub(1).leading_zeros()).clamp(10, 31)
}

fn tuned(old: &[u8], new: &[u8], label: &str, params: &[CParameter]) {
    let started = Instant::now();
    let mut cctx = zstd_safe::CCtx::create();
    for p in params {
        cctx.set_parameter(*p).expect("parameter");
    }
    cctx.ref_prefix(old).expect("prefix");
    let mut out = Vec::with_capacity(zstd_safe::compress_bound(new.len()));
    cctx.compress2(&mut out, new).expect("compress");
    println!(
        "{label:<44} {:>11} bytes {:>7.3}%  {:>6.1}s",
        out.len(),
        out.len() as f64 / new.len() as f64 * 100.0,
        started.elapsed().as_secs_f64()
    );
}

#[test]
#[ignore = "research probe; needs DELTA_PROBE_OLD and DELTA_PROBE_NEW"]
fn compare_zstd_settings() {
    let old_path = std::env::var("DELTA_PROBE_OLD").expect("DELTA_PROBE_OLD");
    let new_path = std::env::var("DELTA_PROBE_NEW").expect("DELTA_PROBE_NEW");
    let old = std::fs::read(&old_path).expect("old");
    let new = std::fs::read(&new_path).expect("new");
    println!("old {} bytes, new {} bytes", old.len(), new.len());

    // The shipped backend, exactly.
    let dir = tempfile::tempdir().expect("temp dir");
    let patch = dir.path().join("p.zst");
    let started = Instant::now();
    ZstdBackend::new()
        .diff(old_path.as_ref(), new_path.as_ref(), &patch)
        .expect("diff");
    let size = std::fs::metadata(&patch).expect("stat").len();
    println!(
        "{:<44} {size:>11} bytes {:>7.3}%  {:>6.1}s",
        "shipped: level 19, window=max, ldm",
        size as f64 / new.len() as f64 * 100.0,
        started.elapsed().as_secs_f64()
    );

    let w = window_log(old.len().max(new.len()));
    let w_sum = window_log(old.len() + new.len());
    tuned(
        &old,
        &new,
        "window=old+new, level 19, ldm",
        &[
            CParameter::CompressionLevel(19),
            CParameter::WindowLog(w_sum),
            CParameter::EnableLongDistanceMatching(true),
        ],
    );
    tuned(
        &old,
        &new,
        "window=max, level 19, no ldm",
        &[CParameter::CompressionLevel(19), CParameter::WindowLog(w)],
    );
    tuned(
        &old,
        &new,
        "window=max, level 19, ldm, hash/chain=window",
        &[
            CParameter::CompressionLevel(19),
            CParameter::WindowLog(w),
            CParameter::HashLog(w.min(30)),
            CParameter::ChainLog(w.min(30)),
            CParameter::EnableLongDistanceMatching(true),
        ],
    );
    tuned(
        &old,
        &new,
        "window=old+new, level 19, ldm, hash/chain",
        &[
            CParameter::CompressionLevel(19),
            CParameter::WindowLog(w_sum),
            CParameter::HashLog(w_sum.min(30)),
            CParameter::ChainLog(w_sum.min(30)),
            CParameter::EnableLongDistanceMatching(true),
        ],
    );
}
