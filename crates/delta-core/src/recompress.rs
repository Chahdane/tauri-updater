//! Rebuilding the exact published `.app.tar.gz` from the exact tar inside it.
//!
//! # Why this has to exist, and why it is hard
//!
//! The minisign signature covers the **compressed** artifact, and
//! `Update::install` consumes that artifact. So patching the tar is only useful
//! if the client can put the gzip layer back byte-for-byte. Anything less and
//! the reconstruction fails its digest gate and the update falls back — correct,
//! but pointless.
//!
//! DEFLATE makes that harder than "use the same compressor at the same level".
//! Its output depends on *when the encoder was handed input*, not only on the
//! input, because the match finder's lookahead is bounded by what has arrived.
//! Feeding the same bytes in different-sized pieces produces different — and
//! usually slightly better — output.
//!
//! # The measured history
//!
//! An earlier probe (`research/experiments/2026-08-13-macos-inprocess-recompression-probe`)
//! compressed the exact tar in one call and recorded a **negative** result: four
//! independent DEFLATE backends all produced an identical 4,070,510-byte
//! candidate against a 4,070,756-byte official artifact, differing 37 bytes into
//! the DEFLATE stream. Four implementations agreeing with each other and all
//! falling 246 bytes short is the signature of a different write pattern, not a
//! different encoder. That probe's conclusion — that a shipped plugin could not
//! do this without running `cargo-tauri` — was correct about the recipe it
//! tested and wrong about the general claim.
//!
//! What was missing is that `tauri-bundler` does not compress a tar. It streams
//! `tar::Builder` **into** the encoder:
//!
//! ```text
//! tar::Builder  ->  flate2::write::GzEncoder  ->  File
//! ```
//!
//! so the encoder sees the archive as a sequence of small writes whose
//! boundaries are the tar's own structure. Replaying those boundaries reproduces
//! the official bytes exactly; chunking the same tar uniformly does not. See
//! [`recompress_app_tar_gz`] for the topology and
//! `research/experiments/2026-08-13-macos-entry-aware-recompression` for the
//! measurement.
//!
//! # What is pinned, and what is only observed
//!
//! [`RECOMPRESSION_TAURI_APP_TAR_GZ_V1`](crate::manifest::RECOMPRESSION_TAURI_APP_TAR_GZ_V1)
//! names the whole recipe, and it is honest about being narrow:
//!
//! | Element | Status |
//! | --- | --- |
//! | The write topology | **Read** from `tar` 0.4.x `builder.rs` and `tauri-bundler` 2.8.1 `updater_bundle.rs` |
//! | `Compression::default()`, mtime 0, OS byte 255 | Read from the same source |
//! | 8192-byte payload chunks | `std::io::copy`'s `DEFAULT_BUF_SIZE`, an implementation detail of the standard library |
//! | flate2 with the **zlib-rs** backend | **Observed.** flate2's default `miniz_oxide` backend produces different bytes and cannot match |
//!
//! None of that is a guarantee anyone offers. It does not have to be: this
//! module is never trusted. Its output is checked against the digest the release
//! published, and a mismatch falls back to a full download like any other delta
//! failure. The recipe identifier exists so that a *future* incompatible recipe
//! gets a new name rather than silently producing wrong bytes under the old one.

use std::io::{Read, Write};
use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::{Error, Result};

/// One tar block.
const BLOCK: usize = 512;

/// `std::io::copy`'s buffer size, which is what sets the payload write
/// boundaries `tar::Builder` produces. Not a public contract of the standard
/// library — see the module docs.
const COPY_CHUNK: usize = 8192;

/// Bytes `tar::Builder::finish` writes to close an archive.
const TRAILER: usize = 1024;

/// Largest tar this will walk without being told otherwise.
///
/// The tar is local and has already passed a digest gate by the time it gets
/// here, so this is a backstop against a malformed *header*, not against a
/// hostile file: a corrupt size field could otherwise describe an entry larger
/// than any real archive.
pub const DEFAULT_MAX_TAR_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Rebuild the published `.app.tar.gz` from the exact tar it contains.
///
/// Reads `tar` in stream order and replays the write topology
/// `tar::Builder` produces into a `GzEncoder`:
///
/// ```text
/// per entry:  write(512-byte header)
///             write(payload) x ceil(size / 8192), the last one short
///             write(padding to the next 512 boundary)   [omitted when exact]
/// finally:    write(1024 zero bytes)                    [the trailer]
///             GzEncoder::finish()
/// ```
///
/// **Not** one write of the whole tar, and **not** uniform chunks of it. Both
/// were measured and both produce different bytes.
///
/// The result is written to `out` and is a *candidate*: nothing here proves it
/// matches what was published, and the caller must check it against the
/// manifest's installer digest before it goes anywhere near an installer.
pub fn recompress_app_tar_gz(tar: &Path, out: &Path) -> Result<()> {
    recompress_with_limit(tar, out, DEFAULT_MAX_TAR_BYTES)
}

/// [`recompress_app_tar_gz`] with an explicit ceiling on the tar's size.
pub fn recompress_with_limit(tar: &Path, out: &Path, max_tar_bytes: u64) -> Result<()> {
    let size = std::fs::metadata(tar)
        .map_err(|e| Error::io("stat", tar, e))?
        .len();
    if size > max_tar_bytes {
        return Err(Error::DeclaredSizeTooLarge {
            declared: size,
            limit: max_tar_bytes,
        });
    }

    let mut source = std::fs::File::open(tar).map_err(|e| Error::io("read", tar, e))?;
    let destination = std::fs::File::create(out).map_err(|e| Error::io("create", out, e))?;
    let mut encoder = GzEncoder::new(destination, Compression::default());

    replay(&mut source, size, &mut encoder).map_err(|e| match e {
        ReplayError::Io(e) => Error::io("recompress", tar, e),
        ReplayError::Malformed(reason) => Error::Manifest(format!("malformed tar: {reason}")),
    })?;

    let mut file = encoder
        .finish()
        .map_err(|e| Error::io("finish", out, std::io::Error::other(e)))?;
    file.flush().map_err(|e| Error::io("flush", out, e))?;
    file.sync_all().map_err(|e| Error::io("sync", out, e))
}

/// Extract the exact tar from a `.app.tar.gz`, refusing to write more than
/// `max_bytes`.
///
/// The inverse of [`recompress_app_tar_gz`], and it lives here so the two stay
/// readable against each other.
///
/// # Why the ceiling is not optional
///
/// The input is a **cached** artifact, and the cache is untrusted on every
/// reuse. gzip is a compression format, so a small file can describe an
/// arbitrarily large output — the classic decompression bomb — and no digest
/// check helps, because the disk is full long before there is anything to hash.
/// `max_bytes` therefore comes from the caller's own policy, and the size the
/// manifest declares is only ever used to *lower* it.
///
/// Written to `out` and returns the number of bytes produced. Exceeding the
/// ceiling stops immediately and removes the partial file, rather than
/// truncating and reporting success.
pub fn decompress_bounded(src: &Path, out: &Path, max_bytes: u64) -> Result<u64> {
    let file = std::fs::File::open(src).map_err(|e| Error::io("read", src, e))?;
    let mut decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut destination = std::fs::File::create(out).map_err(|e| Error::io("create", out, e))?;

    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut written: u64 = 0;
    loop {
        let read = match decoder.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_file(out);
                return Err(Error::io("decompress", src, e));
            }
        };
        // Checked before the write, so the ceiling bounds what reaches the disk
        // rather than what is discovered afterwards.
        if written + read as u64 > max_bytes {
            let _ = std::fs::remove_file(out);
            return Err(Error::OutputTooLarge { limit: max_bytes });
        }
        if let Err(e) = destination.write_all(&buffer[..read]) {
            let _ = std::fs::remove_file(out);
            return Err(Error::io("write", out, e));
        }
        written += read as u64;
    }

    destination
        .flush()
        .map_err(|e| Error::io("flush", out, e))?;
    Ok(written)
}

enum ReplayError {
    Io(std::io::Error),
    Malformed(String),
}

impl From<std::io::Error> for ReplayError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Walk the tar and re-emit it with the writer's boundaries.
fn replay<R: Read, W: Write>(
    source: &mut R,
    total: u64,
    out: &mut W,
) -> std::result::Result<(), ReplayError> {
    let mut header = [0u8; BLOCK];
    let mut payload = vec![0u8; COPY_CHUNK];
    let mut consumed: u64 = 0;

    loop {
        match read_full(source, &mut header)? {
            0 => return Err(ReplayError::Malformed("ended before the trailer".into())),
            BLOCK => {}
            short => {
                return Err(ReplayError::Malformed(format!(
                    "a {short}-byte trailing block is not a tar header"
                )))
            }
        }
        consumed += BLOCK as u64;

        // An all-zero block is the trailer. `Builder::finish` writes exactly
        // 1024 zero bytes and nothing after them, so emitting that rather than
        // copying whatever padding happens to be on disk is what keeps this
        // faithful to the writer rather than to the file.
        if header.iter().all(|b| *b == 0) {
            out.write_all(&[0u8; TRAILER])?;
            return Ok(());
        }

        let declared = parse_size(&header)?;
        let entry_type = header[156];

        // GNU long-name and long-link entries are written as `data` followed by
        // a NUL, through an `io::Chain` — so the payload is one byte longer than
        // the header declares, and it arrives as two writes rather than one.
        // Reproducing that split matters for the same reason the chunk size
        // does. Read out of `tar` 0.4.x `prepare_header_path`; this project has
        // no artifact with a path long enough to exercise it, so it is inferred
        // from source rather than measured.
        let long_name = matches!(entry_type, b'L' | b'K');
        let written = if long_name { declared + 1 } else { declared };

        if written > total.saturating_sub(consumed) {
            return Err(ReplayError::Malformed(format!(
                "entry declares {declared} bytes, more than the archive has left"
            )));
        }

        out.write_all(&header)?;

        // The payload, in the pieces `std::io::copy` hands the encoder.
        let body = if long_name { written - 1 } else { written };
        let mut left = body;
        while left > 0 {
            let want = COPY_CHUNK.min(left as usize);
            if read_full(source, &mut payload[..want])? != want {
                return Err(ReplayError::Malformed("entry payload is truncated".into()));
            }
            out.write_all(&payload[..want])?;
            left -= want as u64;
        }
        if long_name {
            // The chained NUL, which really is a separate write.
            if read_full(source, &mut payload[..1])? != 1 {
                return Err(ReplayError::Malformed("long name is truncated".into()));
            }
            out.write_all(&payload[..1])?;
        }
        consumed += written;

        // One padding write per entry, sized against what was written — never
        // emitted when the payload already lands on a block boundary.
        let remainder = (written % BLOCK as u64) as usize;
        if remainder != 0 {
            let padding = BLOCK - remainder;
            if read_full(source, &mut payload[..padding])? != padding {
                return Err(ReplayError::Malformed("entry padding is truncated".into()));
            }
            out.write_all(&payload[..padding])?;
            consumed += padding as u64;
        }
    }
}

/// The size field: 12 bytes of NUL- or space-terminated octal.
fn parse_size(header: &[u8; BLOCK]) -> std::result::Result<u64, ReplayError> {
    let field = &header[124..136];
    let text = std::str::from_utf8(field)
        .map_err(|_| ReplayError::Malformed("size field is not text".into()))?;
    let text = text.trim_matches(|c| c == '\0' || c == ' ');
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8)
        .map_err(|_| ReplayError::Malformed(format!("size field {text:?} is not octal")))
}

/// Fill `buf`, tolerating short reads, and report how much arrived.
fn read_full<R: Read>(source: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match source.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileHash;

    /// Build a tar the same way `tauri-bundler` does, through the real
    /// `tar::Builder`, so the expected bytes come from the writer rather than
    /// from this module's idea of the writer.
    ///
    /// Only available where the `tar` dev-dependency is, which is everywhere the
    /// test suite runs.
    fn build_reference(files: &[(&str, &[u8])], dir: &Path) -> (Vec<u8>, Vec<u8>) {
        use std::io::Write as _;

        let root = dir.join("src");
        std::fs::create_dir_all(root.join("Contents")).expect("mkdir");
        for (name, body) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            let mut f = std::fs::File::create(&path).expect("create");
            f.write_all(body).expect("write");
        }

        // The bundler's exact call shape: Builder -> GzEncoder -> sink.
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        builder.follow_symlinks(false);
        builder.append_dir_all("Bundle.app", &root).expect("append");
        let gz = builder
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish gzip");

        // And the same archive uncompressed, which is what a tar patch produces.
        let mut builder = tar::Builder::new(Vec::new());
        builder.follow_symlinks(false);
        builder.append_dir_all("Bundle.app", &root).expect("append");
        let tar = builder.into_inner().expect("finish tar");

        (tar, gz)
    }

    fn recompressed(tar: &[u8], dir: &Path) -> Vec<u8> {
        let tar_path = dir.join("exact.tar");
        let out = dir.join("rebuilt.tar.gz");
        std::fs::write(&tar_path, tar).expect("write tar");
        recompress_app_tar_gz(&tar_path, &out).expect("recompress");
        std::fs::read(&out).expect("read result")
    }

    #[test]
    fn reproduces_what_the_bundlers_own_writer_produced() {
        // The central claim, against a reference built by the real
        // `tar::Builder` -> `GzEncoder` pipeline rather than by this module.
        // Payloads are chosen to straddle the boundaries that matter: below one
        // 8192-byte copy chunk, exactly on one, and several chunks with a short
        // final piece.
        let dir = tempfile::tempdir().expect("temp dir");
        let small: Vec<u8> = (0..1015u32).map(|i| (i % 251) as u8).collect();
        let exact: Vec<u8> = (0..COPY_CHUNK as u32).map(|i| (i % 253) as u8).collect();
        let large: Vec<u8> = (0..(COPY_CHUNK as u32 * 3 + 77))
            .map(|i| (i.wrapping_mul(2654435761) % 256) as u8)
            .collect();

        let (tar, official) = build_reference(
            &[
                ("Contents/Info.plist", &small),
                ("Contents/Exactly8192", &exact),
                ("Contents/MacOS/app", &large),
            ],
            dir.path(),
        );

        let rebuilt = recompressed(&tar, dir.path());
        assert_eq!(
            FileHash::of_bytes(&rebuilt),
            FileHash::of_bytes(&official),
            "recompression must reproduce the writer's own bytes exactly \
             (rebuilt {} bytes, official {} bytes)",
            rebuilt.len(),
            official.len(),
        );
    }

    #[test]
    fn the_naive_recipes_really_do_differ() {
        // Without this the test above is unfalsifiable: if any topology
        // reproduced the bytes, replaying entry boundaries would be pointless
        // ceremony rather than the thing that makes it work.
        let dir = tempfile::tempdir().expect("temp dir");
        let body: Vec<u8> = (0..(COPY_CHUNK as u32 * 5 + 13))
            .map(|i| (i.wrapping_mul(2246822519) % 256) as u8)
            .collect();
        let (tar, official) = build_reference(&[("Contents/MacOS/app", &body)], dir.path());

        let one_shot = {
            let mut e = GzEncoder::new(Vec::new(), Compression::default());
            e.write_all(&tar).expect("write");
            e.finish().expect("finish")
        };
        let uniform = {
            let mut e = GzEncoder::new(Vec::new(), Compression::default());
            for chunk in tar.chunks(COPY_CHUNK) {
                e.write_all(chunk).expect("write");
            }
            e.finish().expect("finish")
        };

        assert_ne!(one_shot, official, "one-shot compression must not match");
        assert_ne!(uniform, official, "uniform chunking must not match");
        assert_eq!(
            recompressed(&tar, dir.path()),
            official,
            "only the entry-aware topology matches"
        );
    }

    #[test]
    fn an_empty_file_entry_emits_no_padding_write() {
        // A zero-length payload is a real case (a `.keep`, an empty plist) and
        // the writer emits no padding for it at all. Getting that wrong would
        // add a 512-byte write the encoder never saw.
        let dir = tempfile::tempdir().expect("temp dir");
        let (tar, official) = build_reference(
            &[("Contents/empty", b""), ("Contents/after", b"payload")],
            dir.path(),
        );
        assert_eq!(recompressed(&tar, dir.path()), official);
    }

    #[test]
    fn a_payload_landing_exactly_on_a_block_emits_no_padding() {
        let dir = tempfile::tempdir().expect("temp dir");
        let body = vec![0xABu8; BLOCK * 4];
        let (tar, official) = build_reference(&[("Contents/aligned", &body)], dir.path());
        assert_eq!(recompressed(&tar, dir.path()), official);
    }

    #[test]
    fn a_truncated_tar_is_rejected_rather_than_silently_shortened() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (tar, _) = build_reference(&[("Contents/MacOS/app", &[7u8; 9000])], dir.path());

        // Cut inside the last entry's payload.
        let truncated = &tar[..tar.len() - 4096];
        let path = dir.path().join("truncated.tar");
        std::fs::write(&path, truncated).expect("write");
        let err = recompress_app_tar_gz(&path, &dir.path().join("out.gz"))
            .expect_err("a truncated tar must not produce an artifact");
        assert!(
            matches!(err, Error::Manifest(_)),
            "expected a malformed-tar error, got {err}"
        );
    }

    #[test]
    fn a_corrupt_size_field_is_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (mut tar, _) = build_reference(&[("Contents/MacOS/app", &[3u8; 2048])], dir.path());
        // Find the first entry with a non-zero size and make its size field
        // describe an archive far larger than the file.
        let mut at = 0;
        while at + BLOCK <= tar.len() {
            let size = parse_size(&tar[at..at + BLOCK].try_into().expect("block")).ok();
            if size == Some(0) {
                at += BLOCK;
                continue;
            }
            tar[at + 124..at + 136].copy_from_slice(b"77777777777\0");
            break;
        }
        let path = dir.path().join("corrupt.tar");
        std::fs::write(&path, &tar).expect("write");
        assert!(recompress_app_tar_gz(&path, &dir.path().join("out.gz")).is_err());
    }

    #[test]
    fn a_tar_over_the_ceiling_is_refused_before_it_is_walked() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (tar, _) = build_reference(&[("Contents/a", b"x")], dir.path());
        let path = dir.path().join("big.tar");
        std::fs::write(&path, &tar).expect("write");

        let err = recompress_with_limit(&path, &dir.path().join("out.gz"), 16)
            .expect_err("a tar over the ceiling must be refused");
        assert!(matches!(err, Error::DeclaredSizeTooLarge { .. }), "{err}");
    }
}
