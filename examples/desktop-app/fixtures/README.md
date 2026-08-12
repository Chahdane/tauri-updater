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
