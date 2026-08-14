//! Concurrent updates must not be able to touch each other's files.
//!
//! # The blocker this closes
//!
//! `docs/DECISIONS.md` #18 identified three shared filenames — `update.patch`,
//! `update.artifact` and `full.artifact` — and gave the two delta paths a private
//! `tempdir_in` workspace each. The full-download path kept writing to
//! `work_dir/full.artifact`, a fixed name under a directory the only real caller
//! sets to one shared location. That is blocker **B4**.
//!
//! It matters more than the two that were fixed, because falling back to a full
//! download is the **common** path: a cold cache, a legacy signature, a missing
//! patch, or any delta failure lands here. The isolated paths were the rare ones.
//!
//! # Why these tests are shaped like this
//!
//! A concurrency test that merely runs two updates and checks neither errored can
//! pass on a broken implementation, because the usual symptom of the race is a
//! *signature failure* — which looks like a flake. So each thread here installs a
//! **different release**, and asserts it installed **its own bytes**. Overwriting,
//! renaming over, or deleting another transaction's file all show up as a wrong
//! or missing artifact rather than as an intermittent error.
//!
//! `Fetch` also mirrors what `HttpFetch` really does — stream into a sibling
//! `.part`, then rename onto the target — because that doubles the shared names a
//! broken layout races on, and the pause between the two halves makes the window
//! wide enough that the race is reliable rather than lucky.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use minisign::KeyPair;
use tauri_plugin_updater_delta::test_support::{run_update, Context, InstallHandoff, Outcome};
use tauri_updater_delta_core::client::Fetch;
use tauri_updater_delta_core::release_identity::{current_platform, ReleaseIdentity};
use tauri_updater_delta_core::{FileHash, Limits, UpdateIdentity, VerifiedArtifact};

const APP_ID: &str = "dev.example.testapp";
const URL: &str = "https://example.com/app.bin";

/// Writes the body in two halves with a pause between them, through a sibling
/// `.part` file, exactly as `HttpFetch::stream_to` does.
struct SlowFetch(Vec<u8>);

impl Fetch for SlowFetch {
    fn fetch(&self, _url: &str, out: &Path) -> Result<(), String> {
        let partial = out.with_extension("part");
        let (first, second) = self.0.split_at(self.0.len() / 2);

        let mut file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
        file.write_all(first).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;

        // The window. Long enough that every thread is mid-transfer at once.
        std::thread::sleep(Duration::from_millis(25));

        file.write_all(second).map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&partial, out).map_err(|e| e.to_string())
    }
}

/// Thread-safe, because the point of these tests is to run several at once.
#[derive(Default)]
struct SharedHandoff {
    installed: Mutex<Vec<Vec<u8>>>,
}

impl InstallHandoff for SharedHandoff {
    fn install(&self, artifact: &VerifiedArtifact) -> tauri_plugin_updater_delta::Result<()> {
        self.installed
            .lock()
            .expect("handoff lock")
            .push(artifact.as_bytes().to_vec());
        Ok(())
    }
}

/// One signing key; several genuinely different releases under it.
struct Signer {
    key: KeyPair,
    pubkey: String,
}

impl Signer {
    fn new() -> Self {
        let key = KeyPair::generate_encrypted_keypair(Some(String::new())).expect("keypair");
        let pubkey = base64::engine::general_purpose::STANDARD
            .encode(key.pk.to_box().expect("box pk").into_string());
        Self { key, pubkey }
    }

    fn sign(&self, bytes: &[u8], version: &str) -> String {
        let identity = ReleaseIdentity {
            app_id: APP_ID.to_owned(),
            version: version.to_owned(),
            platform: current_platform(),
            representation: "opaque-v1".to_owned(),
            artifact_blake3: FileHash::of_bytes(bytes).to_hex(),
            artifact_size: bytes.len() as u64,
            signed_at: 1_786_637_312,
        };
        base64::engine::general_purpose::STANDARD.encode(
            minisign::sign(
                None,
                &self.key.sk,
                bytes,
                Some(&identity.to_trusted_comment()),
                None,
            )
            .expect("sign")
            .into_string(),
        )
    }
}

fn manifest(version: &str, signature: &str) -> String {
    format!(
        r#"{{"version":"{version}","platforms":{{"{plat}":{{"url":"{URL}","signature":"{signature}"}}}}}}"#,
        plat = current_platform()
    )
}

/// A release big enough that the two halves are meaningfully different files.
fn body(n: usize) -> Vec<u8> {
    format!("release number {n} ").repeat(4096).into_bytes()
}

#[test]
fn concurrent_full_downloads_install_their_own_bytes() {
    const THREADS: usize = 8;

    let signer = Signer::new();
    // ONE work_dir for every thread. This is what a real app has: `work_dir` is
    // a fixed per-app scratch directory, and the isolation has to come from
    // inside the flow rather than from the caller passing different paths.
    let shared = tempfile::tempdir().expect("work dir");

    let releases: Vec<(String, Vec<u8>, String)> = (0..THREADS)
        .map(|i| {
            let version = format!("1.0.{}", i + 1);
            let bytes = body(i);
            let signature = signer.sign(&bytes, &version);
            (version, bytes, signature)
        })
        .collect();

    let failures = Mutex::new(Vec::<String>::new());

    std::thread::scope(|scope| {
        for (version, bytes, signature) in &releases {
            let signer = &signer;
            let shared = &shared;
            let failures = &failures;
            scope.spawn(move || {
                let handoff = SharedHandoff::default();
                let identity = UpdateIdentity::new(
                    "1.0.0",
                    version,
                    "darwin",
                    URL,
                    signature,
                    manifest(version, signature),
                );

                let result = run_update(
                    &identity,
                    &Context {
                        pubkey: &signer.pubkey,
                        base: None,
                        cache: None,
                        app_id: APP_ID,
                        work_dir: shared.path(),
                        limits: Limits::default(),
                    },
                    &SlowFetch(bytes.clone()),
                    &handoff,
                );

                let mut problems = failures.lock().expect("failure lock");
                match result {
                    Ok(Outcome::InstalledFromFullDownload) => {}
                    other => {
                        problems.push(format!("{version}: expected a full install, got {other:?}"))
                    }
                }

                let installed = handoff.installed.lock().expect("handoff lock");
                if installed.len() != 1 {
                    problems.push(format!(
                        "{version}: expected exactly one install, saw {}",
                        installed.len()
                    ));
                } else if installed[0] != *bytes {
                    // The race, caught. Another transaction's artifact reached
                    // this transaction's installer.
                    problems.push(format!(
                        "{version}: installed {} bytes that are not its own release",
                        installed[0].len()
                    ));
                }
            });
        }
    });

    let problems = failures.into_inner().expect("failure lock");
    assert!(
        problems.is_empty(),
        "concurrent full downloads interfered with each other:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn a_full_download_leaves_no_workspace_behind() {
    let signer = Signer::new();
    let shared = tempfile::tempdir().expect("work dir");

    let bytes = body(1);
    let signature = signer.sign(&bytes, "1.0.1");
    let handoff = SharedHandoff::default();

    let outcome = run_update(
        &UpdateIdentity::new(
            "1.0.0",
            "1.0.1",
            "darwin",
            URL,
            &signature,
            manifest("1.0.1", &signature),
        ),
        &Context {
            pubkey: &signer.pubkey,
            base: None,
            cache: None,
            app_id: APP_ID,
            work_dir: shared.path(),
            limits: Limits::default(),
        },
        &SlowFetch(bytes.clone()),
        &handoff,
    )
    .expect("the update should succeed");

    assert_eq!(outcome, Outcome::InstalledFromFullDownload);

    let leftovers: Vec<_> = std::fs::read_dir(shared.path())
        .expect("read work dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the full path must not outlive its own transaction: {leftovers:?}"
    );
}

#[test]
fn a_failed_full_download_leaves_nothing_that_could_be_mistaken_for_an_artifact() {
    // A transfer that dies halfway must not leave a plausible-looking file
    // behind for the next transaction — or for a concurrent one — to pick up.
    struct Truncating;
    impl Fetch for Truncating {
        fn fetch(&self, _url: &str, out: &Path) -> Result<(), String> {
            let partial = out.with_extension("part");
            std::fs::write(&partial, b"half a download").map_err(|e| e.to_string())?;
            Err("connection reset".to_owned())
        }
    }

    let signer = Signer::new();
    let shared = tempfile::tempdir().expect("work dir");
    let bytes = body(1);
    let signature = signer.sign(&bytes, "1.0.1");
    let handoff = SharedHandoff::default();

    let result = run_update(
        &UpdateIdentity::new(
            "1.0.0",
            "1.0.1",
            "darwin",
            URL,
            &signature,
            manifest("1.0.1", &signature),
        ),
        &Context {
            pubkey: &signer.pubkey,
            base: None,
            cache: None,
            app_id: APP_ID,
            work_dir: shared.path(),
            limits: Limits::default(),
        },
        &Truncating,
        &handoff,
    );

    assert!(
        result.is_err(),
        "a dead transfer is not a successful update"
    );
    assert!(
        handoff.installed.lock().expect("lock").is_empty(),
        "nothing may reach the installer"
    );

    let leftovers: Vec<_> = std::fs::read_dir(shared.path())
        .expect("read work dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a failed download must leave no residue: {leftovers:?}"
    );
}
