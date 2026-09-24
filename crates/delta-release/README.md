# tauri-updater-delta-release

Release-time tooling for
[`tauri-plugin-updater-delta`](https://crates.io/crates/tauri-plugin-updater-delta).
Runs on CI, once per release; nothing here runs on a user's machine.

Two binaries:

- `delta-release` — generate the patches, sign the target installer with an
  authenticated release identity, and write a manifest that is simultaneously a
  valid Tauri updater document.
- `release-check` — read that manifest back as a stranger would, hash the
  artifact about to be uploaded, verify the signature under the configured
  public key, and refuse to publish if the document does not describe the
  release being published.

```sh
cargo install tauri-updater-delta-release

delta-release \
  --platform darwin-aarch64 \
  --app-id com.example.app \
  --target-version 1.0.1 \
  --from-version 1.0.0 \
  --previous-installer prev/App.app.tar.gz \
  --new-installer dist/App.app.tar.gz \
  --installer-url https://releases.example.com/App.app.tar.gz \
  --patch-url https://releases.example.com/1.0.0-to-1.0.1.zst \
  --patch-out dist/1.0.0-to-1.0.1.zst \
  --manifest dist/manifest.json
```

Every generated patch is applied before its metadata is written: a manifest
never describes a patch nobody has proven reconstructs the release.

Direct patches must also earn their download. By default, a patch is published
only when it is strictly smaller than 30% of Full; an oversized patch is deleted
and the manifest remains a valid Full-only release. CI can add
`--require-direct-patch` when missing that target must fail the build.

Pre-release software. See the
[repository](https://github.com/Chahdane/tauri-updater) for the full release
procedure.

MIT licensed.
