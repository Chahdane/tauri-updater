//! Deterministic AppImage-shaped fixtures.
//!
//! Real AppImages are an ELF runtime stub followed by a squashfs image, and that
//! squashfs image is *compressed*. That detail is what makes this fixture honest:
//! the bulk of the file is high-entropy data that zstd cannot shrink on its own,
//! so a small patch can only come from referencing the old artifact. A fixture
//! made of compressible filler would flatter the engine and prove nothing.
//!
//! Generated rather than downloaded, so the suite runs offline and produces
//! identical numbers on every machine.

use std::path::{Path, PathBuf};

/// ELF runtime stub — identical across releases.
const STUB_LEN: usize = 128 * 1024;
/// Payload preceding the app binary. Unchanged between the two versions.
const ASSETS_HEAD_LEN: usize = 3 * 1024 * 1024;
/// The app binary itself. Entirely rewritten by a release.
const BINARY_LEN: usize = 256 * 1024;
/// A newly added asset, inserted mid-file so everything after it shifts.
const ADDED_LEN: usize = 128 * 1024;
/// Payload following the app binary. Unchanged between the two versions.
const ASSETS_TAIL_LEN: usize = 3 * 1024 * 1024;

/// Two versions of the same synthetic AppImage.
pub struct Fixture {
    /// The installed version — the base a patch is applied against.
    pub old: PathBuf,
    /// The released version — what the patch must reconstruct.
    pub new: PathBuf,
}

/// Build a v1/v2 AppImage pair inside `dir`.
///
/// The difference between them models a routine release: the runtime stub and
/// the bulk of the payload are byte-identical, the app binary is rewritten, and
/// one new asset is inserted so that all following bytes shift position.
pub fn appimage_pair(dir: &Path) -> Fixture {
    let stub = elf_stub();
    let assets_head = pseudo_random(0xA55E_5EED, ASSETS_HEAD_LEN);
    let assets_tail = pseudo_random(0x7A11_5EED, ASSETS_TAIL_LEN);
    let binary_v1 = pseudo_random(0x0100_0000, BINARY_LEN);
    let binary_v2 = pseudo_random(0x0101_0000, BINARY_LEN);
    let added = pseudo_random(0x0ADD_ED00, ADDED_LEN);

    let old = concat(&[&stub, &assets_head, &binary_v1, &assets_tail]);
    let new = concat(&[&stub, &assets_head, &binary_v2, &added, &assets_tail]);

    // Every round-trip test in the suite reduces to "these two files can be
    // reconciled". If a change here ever made them identical, all of it would
    // still pass while proving nothing, so the fixture polices itself.
    // Compared with `!=` rather than assert_ne! to avoid dumping 6 MiB of bytes
    // into a panic message.
    assert!(
        old != new,
        "fixture versions are identical — every round-trip test would be vacuous"
    );

    let old_path = dir.join("app_1.0.0.AppImage");
    let new_path = dir.join("app_1.0.1.AppImage");
    std::fs::write(&old_path, &old).expect("write old fixture");
    std::fs::write(&new_path, &new).expect("write new fixture");

    Fixture {
        old: old_path,
        new: new_path,
    }
}

/// Write `contents` to `dir/name` and return the path.
pub fn write(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write fixture");
    path
}

/// Size of a file in bytes.
pub fn size_of(path: &Path) -> u64 {
    std::fs::metadata(path).expect("stat file").len()
}

/// An ELF header carrying the AppImage type-2 magic, then deterministic filler.
fn elf_stub() -> Vec<u8> {
    let mut stub = pseudo_random(0xE1F0_0000, STUB_LEN);
    stub[..8].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    // AppImage type 2 marks itself with "AI\x02" at offset 8.
    stub[8..11].copy_from_slice(&[b'A', b'I', 0x02]);
    stub
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

/// `len` bytes of high-entropy data, reproducible from `seed`.
///
/// xorshift64*: not a good random number generator, but deterministic across
/// platforms and compiler versions, which is what a fixture needs.
fn pseudo_random(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let word = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let take = (len - out.len()).min(8);
        out.extend_from_slice(&word.to_le_bytes()[..take]);
    }
    out
}
