# Windows NSIS compression: options and trade-offs

Status: **investigation only.** Nothing described as an option here is built.
The decision is the maintainer's; this records the evidence for it.

## What is measured

Two settings have real numbers, both from `cargo tauri build` of the example app
on `windows-latest` (tauri-cli 2.10.1), with only the version string changing
between releases (see F38 and F40 in [FINDINGS.md](FINDINGS.md)):

| Setting | Full installer | Direct patch | Patch / Full | Bytes per delta update |
| --- | ---: | ---: | ---: | ---: |
| Solid LZMA (Tauri default), CI run 139 | 3,171,360 | 3,115,925 | 98.25% | 3.12 MB |
| `"compression": "none"`, CI run 36025510400 | 12,658,708 | 640,768 | 5.06% | 0.64 MB |

So, for this app: `none` makes every delta update **4.9× cheaper** and every
Full download **4.0× more expensive**. An installation that takes its first
update as Full pays about 9.5 MB extra once and saves about 2.5 MB on each later
update, so it breaks even after about four delta updates.

Not yet measured, and why it matters:

- **A realistic change.** A version-string change is the smallest possible
  release. `e2e/measure-nsis-compression.sh` adds a rewritten page and a new
  64 KB script (Tauri embeds frontend assets in the executable); item 6's
  benchmark adds tens of MB of assets. Until those run, the 5% is a floor, not a
  typical value.
- **zlib, per-file LZMA, and a compressed Full transport.** Measured by the same
  script. Run it with the manual `NSIS compression study` workflow.

## The options

### A. Solid LZMA (Tauri's default)

Smallest Full. A small source change reshuffles the whole compressed stream, so
direct patches cost roughly as much as Full. The delta path works but saves
nothing. **Reject for delta users**; it is what v0.1 moved away from.

### B. Solid zlib or bzip2

Tauri's template forces `SetCompressor /SOLID` for every algorithm
(`tauri-bundler` 2.8.1 `installer.nsi`, line 13), so these have the same
structural problem as A with a worse ratio. **Expected to be strictly worse
than A**; the study measures it so this is not left as an assumption.

### C. Per-file (non-solid) LZMA

Each file is compressed separately, so unchanged files keep identical
compressed bytes. That helps only if most bytes live in files that do not
change. In a Tauri app the frontend is embedded in the main executable, so the
main executable changes on essentially every release and usually dominates the
installer. **Hypothesis:** patches remain a large fraction of Full for typical
Tauri apps and help only apps with large, stable side-loaded resources. It
needs a custom NSIS template, because Tauri's config cannot express it, and that
template must then be kept in sync with Tauri's by hand.

### D. An inner representation for NSIS (the macOS approach)

Patch the uncompressed payload and rebuild the exact compressed installer on
the client, as `tauri-app-tar-gz-v1` does for `.app.tar.gz`. For NSIS that means
reproducing byte-for-byte:

- the NSIS stub, header and CRC layout for this makensis version;
- makensis's own LZMA encoder (the 7-Zip SDK C code with NSIS's parameters),
  not a Rust LZMA library, whose output differs;
- any Authenticode signature applied after makensis, which the patch would have
  to carry as opaque bytes.

On macOS the recipe was found by reading one Rust writer (#26). Here it is a C
compressor, a binary container format and a signing step, all pinned to a
toolchain the plugin does not control. It is **high-effort and fragile**, and
it buys roughly what E buys more cheaply. **Not recommended.**

### E. Keep `none`, and compress the Full *transport* (recommended to evaluate)

Keep the installer uncompressed, so direct patches stay small, and also publish
a compressed copy of it (for example `setup.exe.zst`) as an optional manifest
field next to the existing Full URL. A plugin client that falls back to Full
downloads the compressed copy, decompresses it with the existing bounded zstd
path (bounded by `target_installer_size` under `Limits::max_target_bytes`), and
verifies the **decompressed** bytes against the same digest, signature and
release identity as today. Stock Tauri clients ignore the field and download
the uncompressed installer exactly as now.

- **Security:** unchanged. What is installed is still the signed installer, and
  a bad compressed copy is an ordinary fallback to the uncompressed URL. The
  field is additive, the same pattern as the tar layer (#25).
- **Expected size:** close to solid LZMA for the Full path (the study's
  `full_transport_estimates.zstd_19` measures it), while delta updates keep the
  5% ratio.
- **Cost:** one more release asset, a bounded decompression on the client, and
  a manifest field with its own tests. First installs from a website are
  unaffected: serve a separately built LZMA installer there if its size
  matters, since website installs have no cache either way.

## Recommendation

Keep `compression: "none"` for the updater artifact (A, B and D are dominated),
run the study to replace the estimates above with measurements, and decide on E
once `zstd_19` for the uncompressed installer is known. If E's measured Full
transport is within roughly 10% of solid LZMA, it removes the Windows
Full-download penalty without touching the delta path. C is worth measuring and
probably not worth maintaining a forked template for.

**Stopped here, as requested.** Nothing in options C–E is implemented.
