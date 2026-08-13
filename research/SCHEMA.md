# Experiment record schema

One JSON object per run, at `experiments/<experiment_id>.json`.

Every field below is **required to be present**. A field whose value is not known
is `null` — never omitted, never estimated. An omitted field looks like an
oversight; an explicit `null` is a record that this was not measured, which is
itself information.

## Fields

| Field | Type | Notes |
| --- | --- | --- |
| `experiment_id` | string | Matches the filename. `YYYY-MM-DD-short-slug`. |
| `date` | string | RFC 3339, when the run happened. |
| `source_commit` | string | Full SHA, from a clean tree. See `tree_dirty`. |
| `tree_dirty` | bool | `true` if the working tree had uncommitted changes. A dirty-tree measurement is not reproducible and the ledger must not cite it as demonstrated. |
| `platform` | string | `linux` / `darwin` / `windows`. |
| `os_version` | string \| null | e.g. `macOS 15.3`. |
| `architecture` | string | `x86_64` / `aarch64`. |
| `rust_version` | string \| null | `rustc --version`. |
| `tauri_version` | string \| null | Resolved, from `Cargo.lock`. |
| `tauri_plugin_updater_version` | string \| null | Resolved. Relevant because the security claims are pinned to it (DECISIONS #21). |
| `zstd_version` | string \| null | Crate version, and the bundled libzstd if known. |
| `bundler_versions` | object \| null | e.g. `{"tauri-cli": "2.1.0", "appimagetool": null}`. |
| `old_version` | string | Version patched **from**. |
| `new_version` | string | Version patched **to**. |
| `version_distance` | string \| null | `patch` / `minor` / `major`, or a count. |
| `old_artifact_hash` | string | BLAKE3, lowercase hex. The artifact must still exist or be reproducible. |
| `new_artifact_hash` | string | Same. |
| `old_artifact_size` | integer | Bytes. |
| `new_artifact_size` | integer | Bytes. |
| `patch_hash` | string | BLAKE3 of the patch. |
| `patch_size` | integer | Bytes. |
| `patch_ratio` | number | `patch_size / new_artifact_size`. Recorded rather than derived at read time so a mistake is visible. |
| `representation` | string | **What was actually patched.** e.g. `app.tar.gz`, `tar`, `AppImage`, `raw-binary`. The single most important field for interpreting a ratio — see FINDINGS F4/F5. |
| `compression_settings` | object \| null | e.g. `{"zstd_level": 19, "window_log": 27}`. |
| `patch_generation_seconds` | number \| null | |
| `patch_application_seconds` | number \| null | |
| `peak_memory_bytes` | integer \| null | Only if actually measured. |
| `disk_usage_bytes` | integer \| null | Only if actually measured. |
| `source_change_category` | string \| null | `string-only` / `single-function` / `dependency-bump` / `feature` / `unknown`. |
| `changed_loc` | integer \| null | Only if counted. |
| `notes` | string \| null | Anything that would change how a reader interprets the numbers. Confounders belong here. |
| `raw_log` | string \| null | Path relative to `research/`, e.g. `logs/2026-08-13-foo.log`. |
| `artifact_provenance` | string | How the artifacts were produced, precisely enough to repeat. `"unknown"` is permitted and disqualifies the record from supporting a DEMONSTRATED finding. |

## Template

```json
{
  "experiment_id": "YYYY-MM-DD-slug",
  "date": null,
  "source_commit": null,
  "tree_dirty": null,
  "platform": null,
  "os_version": null,
  "architecture": null,
  "rust_version": null,
  "tauri_version": null,
  "tauri_plugin_updater_version": null,
  "zstd_version": null,
  "bundler_versions": null,
  "old_version": null,
  "new_version": null,
  "version_distance": null,
  "old_artifact_hash": null,
  "new_artifact_hash": null,
  "old_artifact_size": null,
  "new_artifact_size": null,
  "patch_hash": null,
  "patch_size": null,
  "patch_ratio": null,
  "representation": null,
  "compression_settings": null,
  "patch_generation_seconds": null,
  "patch_application_seconds": null,
  "peak_memory_bytes": null,
  "disk_usage_bytes": null,
  "source_change_category": null,
  "changed_loc": null,
  "notes": null,
  "raw_log": null,
  "artifact_provenance": null
}
```

## Why `representation` and `artifact_provenance` carry so much weight

The two observations this project already has — roughly 95% on a compressed
`.app.tar.gz` and roughly 6.6% on a related tar-layer experiment — differ by more
than an order of magnitude, and the most obvious explanation is that they patched
*different representations of the same thing*.

But that explanation is not established, because provenance was not controlled:
the executables themselves differed substantially, timestamps and metadata
differed, and deterministic recompression was never demonstrated. Any of those
could account for some of the gap.

So a record that cannot say exactly what was patched and exactly how it was built
cannot support a causal claim, no matter how clean its numbers look. Those two
fields are what separates a measurement from a result.
