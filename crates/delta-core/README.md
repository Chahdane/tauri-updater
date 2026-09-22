# tauri-updater-delta-core

The platform-agnostic engine behind
[`tauri-plugin-updater-delta`](https://crates.io/crates/tauri-plugin-updater-delta):
hashing, zstd diff/apply, the release manifest, the authenticated release
identity carried in a minisign trusted comment, signature verification, the
content-addressed artifact cache, local resource ceilings, and exact macOS
`.app.tar.gz` recompression.

It knows nothing about Tauri, about HTTP, or about how an update is installed.
Applications do not normally depend on it directly — add the plugin instead.

```rust
use tauri_updater_delta_core::{backend::backend_for, hash::{verify_file, FileHash}};

let backend = backend_for("zstd")?;
backend.apply(base, patch, rebuilt)?;
verify_file(rebuilt, &FileHash::from_hex(expected_blake3)?)?;
```

Applying a patch produces a *candidate* file, never a trusted one: a patch is
untrusted input, and only the hash check establishes that reconstruction
actually succeeded.

Pre-release software. See the
[repository](https://github.com/Chahdane/tauri-updater) for the security model,
the scope of what has been demonstrated, and the evidence behind it.

MIT licensed.
