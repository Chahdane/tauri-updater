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

## Why the E2E harness enables insecure transport

The normal `tauri.conf.json` uses an HTTPS placeholder and does **not** set
`dangerousInsecureTransportProtocol`. The example's default feature set also
does not include `e2e-control` or the plugin's `test-support` feature.

The build scripts temporarily add Tauri's insecure flag while building with
`--features e2e-control`. That feature also makes the delta plugin's localhost
opt-in and endpoint override methods exist. The scripts restore the checked-in
configuration when they exit.

**This is a localhost test harness. Never copy those flags into a real app.**

The harness serves the manifest and artifacts from `http://127.0.0.1` on an
ephemeral port, and both layers refuse plain HTTP in a release build:
`tauri-plugin-updater` refuses it in `validate_endpoints`, and this plugin
refuses it in `HttpFetch`. `cargo tauri build` produces a release binary, so
without the flag the harness cannot run at all.

Neither relaxation is merely set to false in a shipping build: the methods and
control server are absent from the default compilation. See
`docs/DECISIONS.md` #19 and #34.
