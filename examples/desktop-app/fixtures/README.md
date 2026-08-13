# Test signing keys

**No private key lives here, and none should.**

`e2e-test.key.pub` is a placeholder public key so the example app's
`tauri.conf.json` is coherent when built by hand. Nobody holds the matching
private key — it was generated and discarded.

The end-to-end harness generates a **fresh keypair per run** into a temporary
directory, writes the public half into `tauri.conf.json` before building, and
signs the release artifacts with the private half. Nothing secret is ever
written into the repository.

This is deliberate. A test-only private key committed "because it signs nothing
real" is the same class of hazard as a test-only HTTP surface that ships: safe
by intent, dangerous by accident, and impossible to notice once it is old. Not
committing it makes the hazard structurally absent rather than documented.

## Why the example enables `dangerousInsecureTransportProtocol`

`tauri.conf.json` sets `"dangerousInsecureTransportProtocol": true`.

**This is a localhost test harness. Never copy that flag into a real app.**

The harness serves the manifest and artifacts from `http://127.0.0.1` on an
ephemeral port, and both layers refuse plain HTTP in a release build:
`tauri-plugin-updater` refuses it in `validate_endpoints`, and this plugin
refuses it in `HttpFetch`. `cargo tauri build` produces a release binary, so
without the flag the harness cannot run at all.

The flag was always required — it is not a new weakening. Before it was written
down, the harness depended on upstream tolerating `http://` endpoints, which was
already going to fail the moment it ran in release. Making the dependency
explicit is the change; the exposure is the same. See `docs/DECISIONS.md` #19.
