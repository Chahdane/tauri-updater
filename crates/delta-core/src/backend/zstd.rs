//! The zstd patch backend — backend #1, and the default.
//!
//! This is the same technique as the `zstd --patch-from` command line flag, done
//! through the library so nothing has to be installed on the user's machine.
//!
//! The idea is simple: hand zstd the *old* artifact as a compression prefix and
//! raise the window log until it covers that prefix entirely. The compressor can
//! then emit a match against any byte of the old file, so everything the two
//! versions have in common costs a reference instead of a copy. What is left is
//! genuinely new bytes.
//!
//! Decompression must be given the identical prefix, which is what makes the
//! result a patch rather than a standalone archive.
//!
//! # Memory
//!
//! The prefix has to be resident, so both halves use memory proportional to the
//! old artifact. [`apply`](ZstdBackend::apply) streams its *output* to disk, so
//! it does not additionally hold the reconstructed artifact in memory; `diff`
//! runs on CI, where being simpler matters more than being lean.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use ::zstd::zstd_safe::{self, CParameter, DParameter, InBuffer, OutBuffer};

use crate::backend::PatchBackend;
use crate::{Error, Result};

/// Smallest window log zstd accepts.
const WINDOW_LOG_MIN: u32 = 10;

/// Largest window log zstd accepts on this target: 2 GiB on 64-bit, 1 GiB on 32-bit.
const WINDOW_LOG_MAX: u32 = if usize::BITS >= 64 { 31 } else { 30 };

/// Bytes of patch data read per iteration while applying.
const READ_CHUNK: usize = 128 * 1024;

/// Bytes of reconstructed output buffered before each write.
const WRITE_CHUNK: usize = 256 * 1024;

/// Patch backend built on zstd's prefix-referencing compression.
#[derive(Debug, Clone)]
pub struct ZstdBackend {
    level: i32,
    max_output_bytes: u64,
}

impl ZstdBackend {
    /// Identifier recorded in the patch manifest.
    pub const ID: &'static str = "zstd";

    /// Compression level used when producing patches.
    ///
    /// 19 is the highest non-"ultra" level. Diffing happens once per release on
    /// CI while the resulting patch is downloaded by every user, so spending
    /// compression time here is close to free.
    pub const DEFAULT_LEVEL: i32 = 19;

    /// Default ceiling on how much a single patch may reconstruct: 4 GiB.
    ///
    /// A patch is untrusted input. Without a ceiling, a malicious or corrupt one
    /// could be crafted to expand indefinitely and fill the user's disk.
    pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

    /// A backend with the default level and output ceiling.
    pub fn new() -> Self {
        Self {
            level: Self::DEFAULT_LEVEL,
            max_output_bytes: Self::DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    /// Override the compression level used by [`PatchBackend::diff`].
    pub fn with_level(mut self, level: i32) -> Self {
        self.level = level;
        self
    }

    /// Override how many bytes [`PatchBackend::apply`] may write before it gives
    /// up and reports [`Error::OutputTooLarge`].
    pub fn with_max_output_bytes(mut self, bytes: u64) -> Self {
        self.max_output_bytes = bytes;
        self
    }
}

impl Default for ZstdBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl PatchBackend for ZstdBackend {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn diff(&self, old: &Path, new: &Path, patch: &Path) -> Result<()> {
        let old_data = read_file(old)?;
        let new_data = read_file(new)?;

        // The window must span the prefix for matches to reach back into it, and
        // span the new data so long-range matches within it are also available.
        let window_log = window_log_for(old_data.len().max(new_data.len()) as u64);

        let mut cctx = zstd_safe::CCtx::create();
        set_c(&mut cctx, CParameter::CompressionLevel(self.level))?;
        set_c(&mut cctx, CParameter::WindowLog(window_log))?;
        set_c(&mut cctx, CParameter::EnableLongDistanceMatching(true))?;
        cctx.ref_prefix(&old_data)
            .map_err(|code| backend_error("reference the old artifact", code))?;

        let mut buffer = Vec::with_capacity(zstd_safe::compress_bound(new_data.len()));
        cctx.compress2(&mut buffer, &new_data)
            .map_err(|code| backend_error("compress", code))?;

        write_file(patch, &buffer)
    }

    fn apply(&self, old: &Path, patch: &Path, out: &Path) -> Result<()> {
        let old_data = read_file(old)?;

        let mut dctx = zstd_safe::DCtx::create();
        // The frame header carries the window log `diff` chose; permitting the
        // maximum here simply means we do not reject our own patches.
        dctx.set_parameter(DParameter::WindowLogMax(WINDOW_LOG_MAX))
            .map_err(|code| backend_error("configure the decompressor", code))?;
        dctx.ref_prefix(&old_data)
            .map_err(|code| backend_error("reference the old artifact", code))?;

        let mut patch_file = File::open(patch).map_err(|e| Error::io("open", patch, e))?;
        let mut out_file = File::create(out).map_err(|e| Error::io("create", out, e))?;

        let mut read_buffer = vec![0u8; READ_CHUNK];
        let mut write_buffer = Vec::with_capacity(WRITE_CHUNK);
        let mut written: u64 = 0;
        let mut frame_complete = false;

        loop {
            let read = patch_file
                .read(&mut read_buffer)
                .map_err(|e| Error::io("read", patch, e))?;
            if read == 0 {
                break;
            }

            let mut input = InBuffer::around(&read_buffer[..read]);
            while input.pos() < read {
                if frame_complete {
                    return Err(backend_message(
                        "apply",
                        "trailing data after the patch frame",
                    ));
                }

                write_buffer.clear();
                let mut output = OutBuffer::around(&mut write_buffer);
                let consumed_before = input.pos();

                let hint = dctx
                    .decompress_stream(&mut output, &mut input)
                    .map_err(|code| backend_error("apply", code))?;

                let produced = output.as_slice();
                written += produced.len() as u64;
                if written > self.max_output_bytes {
                    return Err(Error::OutputTooLarge {
                        limit: self.max_output_bytes,
                    });
                }
                out_file
                    .write_all(produced)
                    .map_err(|e| Error::io("write", out, e))?;

                if hint == 0 {
                    frame_complete = true;
                    break;
                }
                // Neither side advanced, so looping again would spin forever.
                if produced.is_empty() && input.pos() == consumed_before {
                    return Err(backend_message("apply", "decompressor stalled"));
                }
            }
        }

        if !frame_complete {
            return Err(Error::TruncatedPatch);
        }

        out_file.flush().map_err(|e| Error::io("flush", out, e))?;
        Ok(())
    }
}

/// Smallest window log whose window spans `size` bytes, clamped to what zstd
/// accepts on this target.
fn window_log_for(size: u64) -> u32 {
    // ceil(log2(size)), with size 0 and 1 both landing on the minimum.
    let bits = u64::BITS - size.max(1).saturating_sub(1).leading_zeros();
    bits.clamp(WINDOW_LOG_MIN, WINDOW_LOG_MAX)
}

fn set_c(cctx: &mut zstd_safe::CCtx<'_>, param: CParameter) -> Result<()> {
    cctx.set_parameter(param)
        .map(|_| ())
        .map_err(|code| backend_error("configure the compressor", code))
}

fn backend_error(operation: &'static str, code: usize) -> Error {
    backend_message(operation, zstd_safe::get_error_name(code))
}

fn backend_message(operation: &'static str, message: impl Into<String>) -> Error {
    Error::Backend {
        backend: ZstdBackend::ID,
        operation,
        message: message.into(),
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::io("read", path, e))
}

fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(path, data).map_err(|e| Error::io("write", path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_log_covers_the_prefix() {
        assert_eq!(window_log_for(0), WINDOW_LOG_MIN);
        assert_eq!(window_log_for(1), WINDOW_LOG_MIN);
        // Anything at or below the minimum window stays there.
        assert_eq!(window_log_for(1 << WINDOW_LOG_MIN), WINDOW_LOG_MIN);
        // One byte over needs the next power of two.
        assert_eq!(
            window_log_for((1 << WINDOW_LOG_MIN) + 1),
            WINDOW_LOG_MIN + 1
        );
        assert_eq!(window_log_for(1 << 20), 20);
        assert_eq!(window_log_for((1 << 20) + 1), 21);
        // Oversized inputs clamp rather than producing a value zstd rejects.
        assert_eq!(window_log_for(u64::MAX), WINDOW_LOG_MAX);
    }
}
