//! Content hashing and verification.
//!
//! Every delta install ends with the same question: *is the file we just
//! rebuilt bit-for-bit the file the release process published?* This module
//! answers it. If the answer is no, the caller throws the reconstruction away
//! and downloads the full artifact instead.
//!
//! BLAKE3 is used because artifacts are large — hundreds of megabytes for a
//! desktop installer — and it hashes them several times faster than SHA-256
//! while remaining a 256-bit cryptographic digest.
//!
//! This is a *correctness* check, not the security boundary — it proves the
//! reconstruction produced the file the manifest described, nothing more.
//!
//! Authenticity is established solely by the minisign signature over the
//! **target artifact**, checked in [`crate::signature`] before the bytes reach
//! an installer. Two things this module must not be read as claiming:
//!
//! - **There is no signature over the release manifest.** The manifest is not
//!   authenticated at all; matching a digest it published proves only internal
//!   consistency. See `docs/DECISIONS.md` #11 and #13.
//! - **Tauri does not re-check the artifact we hand back.** `Update::install`
//!   verifies nothing; verification lives in `Update::download`, which the delta
//!   path exists to skip. See `docs/DECISIONS.md` #10.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::{Error, Result};

/// Bytes pulled from disk per read while hashing.
const CHUNK_SIZE: usize = 64 * 1024;

/// Length of a digest in hex characters.
const HEX_LEN: usize = 64;

/// A BLAKE3 digest of a file's contents.
///
/// Comparison is constant-time, so a [`FileHash`] can be compared against an
/// untrusted value without leaking a timing signal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FileHash(blake3::Hash);

impl FileHash {
    /// Hash an in-memory buffer.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes))
    }

    /// Hash a file by streaming it, so memory use stays flat regardless of size.
    pub fn of_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path).map_err(|e| Error::io("open", path, e))?;

        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; CHUNK_SIZE];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|e| Error::io("read", path, e))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }

        Ok(Self(hasher.finalize()))
    }

    /// Parse a lowercase hex digest, as stored in a release manifest.
    pub fn from_hex(value: &str) -> Result<Self> {
        if value.len() != HEX_LEN {
            return Err(Error::InvalidHash {
                value: value.to_owned(),
                reason: format!("expected {HEX_LEN} hex characters, got {}", value.len()),
            });
        }
        blake3::Hash::from_hex(value)
            .map(Self)
            .map_err(|e| Error::InvalidHash {
                value: value.to_owned(),
                reason: e.to_string(),
            })
    }

    /// The digest as a lowercase hex string.
    pub fn to_hex(self) -> String {
        self.0.to_hex().to_string()
    }

    /// The raw 32 digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl fmt::Display for FileHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_hex())
    }
}

impl fmt::Debug for FileHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileHash({})", self.0.to_hex())
    }
}

/// Check that `path` hashes to `expected`.
///
/// This is the gate every reconstructed artifact passes through before it is
/// handed to an installer.
pub fn verify_file(path: impl AsRef<Path>, expected: &FileHash) -> Result<()> {
    let path = path.as_ref();
    let actual = FileHash::of_file(path)?;
    if actual == *expected {
        Ok(())
    } else {
        Err(Error::ChecksumMismatch {
            path: path.to_path_buf(),
            expected: expected.to_hex(),
            actual: actual.to_hex(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write `contents` to a uniquely-named file in the test temp directory.
    fn temp_file(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("delta-core-hash-{name}"));
        let mut file = File::create(&path).expect("create temp file");
        file.write_all(contents).expect("write temp file");
        path
    }

    #[test]
    fn hashes_a_known_vector() {
        // Official BLAKE3 test vector for the input "abc".
        assert_eq!(
            FileHash::of_bytes(b"abc").to_hex(),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }

    #[test]
    fn file_and_buffer_hashes_agree() {
        // Larger than CHUNK_SIZE so the streaming loop runs more than once.
        let contents: Vec<u8> = (0..(CHUNK_SIZE * 3 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        let path = temp_file("streaming", &contents);

        assert_eq!(
            FileHash::of_file(&path).expect("hash file"),
            FileHash::of_bytes(&contents)
        );
    }

    #[test]
    fn hashes_an_empty_file() {
        let path = temp_file("empty", b"");
        assert_eq!(
            FileHash::of_file(&path).expect("hash file"),
            FileHash::of_bytes(b"")
        );
    }

    #[test]
    fn hex_round_trips() {
        let hash = FileHash::of_bytes(b"round trip");
        assert_eq!(FileHash::from_hex(&hash.to_hex()).expect("parse"), hash);
    }

    #[test]
    fn rejects_malformed_hex() {
        for bad in ["", "abc", &"z".repeat(HEX_LEN)] {
            assert!(
                matches!(FileHash::from_hex(bad), Err(Error::InvalidHash { .. })),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn verify_accepts_a_matching_file() {
        let path = temp_file("verify-ok", b"payload");
        verify_file(&path, &FileHash::of_bytes(b"payload")).expect("should verify");
    }

    #[test]
    fn verify_rejects_a_modified_file() {
        let path = temp_file("verify-bad", b"payload");
        let err =
            verify_file(&path, &FileHash::of_bytes(b"different")).expect_err("should not verify");
        assert!(matches!(err, Error::ChecksumMismatch { .. }));
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let err = FileHash::of_file("/definitely/not/a/real/path").expect_err("should fail");
        assert!(matches!(
            err,
            Error::Io {
                operation: "open",
                ..
            }
        ));
    }
}
