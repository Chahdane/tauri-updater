# tauri-plugin-updater-delta

Differential updates for Tauri v2. The plugin keeps Tauri's official update
check and installer, but may reconstruct the exact published artifact from a
much smaller patch before handing it to `tauri-plugin-updater`.

```rust
use tauri_plugin_updater_delta::DeltaUpdaterExt;

async fn update(app: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(update) = app.delta_updater().check().await? {
        let outcome = update.install().await?;
        println!("installed via {}", outcome.path_name());
    }
    Ok(())
}
```

Register it alongside `tauri_plugin_updater` and configure the endpoint and
public key in `tauri.conf.json` as usual. There is no second manifest fetch and
no delta-specific configuration.

- The delta path is an optimisation, never a dependency: a cache miss, a
  missing patch, or a failed reconstruction falls back to the full download the
  official updater would have performed anyway.
- The final artifact's signature is verified inside this plugin, because the
  pinned upstream `Update::install` does not verify what it is handed.
- Downgrades and authenticated release-identity contradictions fail closed and
  never fall back.

Pre-release software. See the
[repository](https://github.com/Chahdane/tauri-updater) for the supported
platforms, the security model, and the evidence behind every claim.

MIT licensed.
