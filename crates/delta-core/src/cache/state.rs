//! The cache's only mutable state, and the one place concurrency can bite.
//!
//! # Why atomic replacement is not enough
//!
//! Blobs are content-addressed and immutable, so two processes writing the same
//! artifact write identical bytes and races over them are harmless. All the
//! mutable state is here: which entry is ACTIVE and which is PENDING.
//!
//! Replacing that state atomically stops a *torn* file. It does not stop a
//! **lost update**, because every transition is a read-modify-write:
//!
//! ```text
//! P1 reads {active: v1, pending: v2}   P2 reads {active: v1, pending: v2}
//! P1 promotes  -> {active: v2}
//!                                      P2 stages v3 -> {active: v1, pending: v3}
//!                                         ^ P1's promotion is gone
//! ```
//!
//! Both writes were atomic. The second was computed from state that had already
//! changed, so it silently reverted ACTIVE from v2 to v1.
//!
//! # The fix: compare-and-set on a generation number
//!
//! State lives in `state.<generation>.json`. The current state is the highest
//! generation present. A writer that read generation *N* may only create
//! generation *N+1*, and creation **fails if that file already exists** — so of
//! two racing writers exactly one wins and the loser re-reads and retries
//! against the state that actually won. A lost update stops being possible
//! rather than becoming unlikely.
//!
//! # Why `hard_link` rather than `rename`
//!
//! `rename` is the obvious primitive and it is the wrong one, because its
//! semantics differ by platform:
//!
//! | | destination exists |
//! | --- | --- |
//! | POSIX `rename(2)` | silently replaces — no CAS, lost updates return |
//! | Windows `MoveFileW` | fails |
//!
//! Building on `rename` would produce a design that happens to work on one
//! platform and silently loses updates on the other. `hard_link` fails with
//! `AlreadyExists` on **both**, which is exactly the create-if-absent primitive
//! a CAS needs. The sequence is: write a temp file, flush it, link it into place
//! under the next generation name, unlink the temp. A crash at any point leaves
//! either the old generation or the new one complete — never a partial file
//! under a generation name, because the name is only ever created by a link to
//! an already-complete file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Version of the on-disk cache layout.
pub const CACHE_FORMAT_VERSION: u32 = 1;

/// Everything a cache entry must agree with before it may be reused.
///
/// A mismatch on any field means the cached artifact does not describe this
/// installation, so the cache is treated as empty rather than repaired. Key
/// fingerprint is included so that rotating the signing key invalidates the
/// cache instead of leaving artifacts signed by a retired key reusable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Namespace {
    /// Application bundle identifier.
    pub bundle_id: String,
    /// Tauri platform string, e.g. `darwin-aarch64`.
    pub platform: String,
    /// Target architecture.
    pub arch: String,
    /// BLAKE3 of the configured public key, so a key rotation empties the cache.
    pub pubkey_fingerprint: String,
    /// Representation identifier from the manifest's tar layer.
    pub representation: String,
    /// Recompression recipe identifier.
    pub recompression: String,
}

/// One cached artifact, identified by content rather than by filename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Application version this artifact installs.
    pub version: String,
    /// BLAKE3 of the compressed installer, and the blob's content address.
    pub compressed_blake3: String,
    /// Size of the compressed installer.
    pub compressed_size: u64,
    /// BLAKE3 of the exact tar inside it.
    pub tar_blake3: String,
    /// Size of that tar.
    pub tar_size: u64,
}

/// The cache's mutable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheState {
    /// On-disk layout version.
    pub format_version: u32,
    /// What this cache is bound to.
    pub namespace: Namespace,
    /// The artifact for the version believed to be installed.
    pub active: Option<CacheEntry>,
    /// A verified artifact handed to the installer, awaiting confirmation that
    /// the application actually came back up running it.
    pub pending: Option<CacheEntry>,
}

impl CacheState {
    /// An empty state for `namespace`.
    pub fn empty(namespace: Namespace) -> Self {
        Self {
            format_version: CACHE_FORMAT_VERSION,
            namespace,
            active: None,
            pending: None,
        }
    }
}

/// A read of the state, carrying the generation it was read at.
///
/// The generation is the whole point: it is what a later write is checked
/// against, so it travels with the value rather than being fetched again.
#[derive(Debug, Clone)]
pub struct Versioned {
    /// Generation this was read at.
    pub generation: u64,
    /// The state itself.
    pub state: CacheState,
}

/// Why a compare-and-set did not apply.
#[derive(Debug)]
pub enum CasError {
    /// Another writer got there first. Re-read and retry.
    Conflict,
    /// Something else went wrong.
    Failed(Error),
}

/// The generation-numbered state directory.
pub struct StateStore {
    dir: PathBuf,
}

impl StateStore {
    /// Open (or create) a store rooted at `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| Error::io("create", &dir, e))?;
        Ok(Self { dir })
    }

    fn path_for(&self, generation: u64) -> PathBuf {
        self.dir.join(format!("state.{generation}.json"))
    }

    /// Highest generation present, and every generation found.
    fn generations(&self) -> Result<Vec<u64>> {
        let mut found = Vec::new();
        let entries = std::fs::read_dir(&self.dir).map_err(|e| Error::io("read", &self.dir, e))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rest) = name.strip_prefix("state.") {
                if let Some(number) = rest.strip_suffix(".json") {
                    if let Ok(generation) = number.parse::<u64>() {
                        found.push(generation);
                    }
                }
            }
        }
        found.sort_unstable();
        Ok(found)
    }

    /// Read the current state, or `None` if the store is empty.
    ///
    /// A generation that fails to parse is skipped rather than fatal: a
    /// half-written file cannot occur under a generation name by construction,
    /// but a corrupted disk can still produce one, and refusing to read the
    /// cache at all would be a worse outcome than falling back a generation.
    pub fn load(&self) -> Result<Option<Versioned>> {
        for generation in self.generations()?.into_iter().rev() {
            let path = self.path_for(generation);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match serde_json::from_str::<CacheState>(&text) {
                Ok(state) if state.format_version == CACHE_FORMAT_VERSION => {
                    return Ok(Some(Versioned { generation, state }));
                }
                _ => continue,
            }
        }
        Ok(None)
    }

    /// Publish `state` as generation `expected + 1`, failing if that generation
    /// already exists.
    ///
    /// `expected` is the generation the caller read. Passing a stale value is
    /// precisely the lost-update case, and it is what this refuses.
    pub fn compare_and_set(
        &self,
        expected: u64,
        state: &CacheState,
    ) -> std::result::Result<u64, CasError> {
        let next = expected + 1;
        let target = self.path_for(next);

        let text = serde_json::to_string_pretty(state)
            .map_err(|e| CasError::Failed(Error::Manifest(e.to_string())))?;

        // Unique temp name so two racers never collide on the staging file.
        let temp = self.dir.join(format!(
            "state.{next}.{}.{:?}.tmp",
            std::process::id(),
            std::thread::current().id()
        ));

        write_and_sync(&temp, text.as_bytes())
            .map_err(|e| CasError::Failed(Error::io("write", &temp, e)))?;

        // The CAS. Fails with AlreadyExists on every supported platform if some
        // other writer already published this generation.
        let result = std::fs::hard_link(&temp, &target);
        let _ = std::fs::remove_file(&temp);

        match result {
            Ok(()) => Ok(next),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(CasError::Conflict),
            Err(e) => Err(CasError::Failed(Error::io("link", &target, e))),
        }
    }

    /// Initialise an empty store at generation 0, if it has none.
    pub fn initialise(&self, namespace: Namespace) -> Result<Versioned> {
        if let Some(current) = self.load()? {
            return Ok(current);
        }
        let state = CacheState::empty(namespace);
        let path = self.path_for(0);
        let text =
            serde_json::to_string_pretty(&state).map_err(|e| Error::Manifest(e.to_string()))?;
        let temp = self.dir.join(format!("state.0.{}.tmp", std::process::id()));
        write_and_sync(&temp, text.as_bytes()).map_err(|e| Error::io("write", &temp, e))?;
        let linked = std::fs::hard_link(&temp, &path);
        let _ = std::fs::remove_file(&temp);
        match linked {
            Ok(()) => Ok(Versioned {
                generation: 0,
                state,
            }),
            // Someone else initialised first; theirs is as good as ours.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(self
                .load()?
                .expect("a generation exists because linking said so")),
            Err(e) => Err(Error::io("link", &path, e)),
        }
    }

    /// Remove generations below the current one, keeping a few for forensics.
    pub fn prune(&self, keep: usize) -> Result<usize> {
        let generations = self.generations()?;
        if generations.len() <= keep {
            return Ok(0);
        }
        let mut removed = 0;
        for generation in &generations[..generations.len() - keep] {
            if std::fs::remove_file(self.path_for(*generation)).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    // Durable before it is linked into place, so a crash cannot publish a
    // generation whose contents never reached the disk.
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespace() -> Namespace {
        Namespace {
            bundle_id: "com.example.app".to_owned(),
            platform: "darwin-aarch64".to_owned(),
            arch: "aarch64".to_owned(),
            pubkey_fingerprint: "aa".repeat(32),
            representation: "app-tar-gz-v1".to_owned(),
            recompression: "tauri-app-tar-gz-v1".to_owned(),
        }
    }

    fn entry(version: &str) -> CacheEntry {
        CacheEntry {
            version: version.to_owned(),
            compressed_blake3: format!("{version:0>64}").replace('.', "0"),
            compressed_size: 4_070_756,
            tar_blake3: format!("{version:1>64}").replace('.', "1"),
            tar_size: 9_461_248,
        }
    }

    #[test]
    fn an_empty_store_reads_as_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StateStore::open(dir.path()).expect("open");
        assert!(store.load().expect("load").is_none());

        let initial = store.initialise(namespace()).expect("initialise");
        assert_eq!(initial.generation, 0);
        assert!(initial.state.active.is_none());
        assert!(initial.state.pending.is_none());
    }

    #[test]
    fn a_write_advances_the_generation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StateStore::open(dir.path()).expect("open");
        let current = store.initialise(namespace()).expect("initialise");

        let mut next = current.state.clone();
        next.pending = Some(entry("1.0.1"));
        let generation = store
            .compare_and_set(current.generation, &next)
            .expect("first writer wins");
        assert_eq!(generation, 1);

        let reread = store.load().expect("load").expect("state");
        assert_eq!(reread.generation, 1);
        assert_eq!(reread.state.pending, Some(entry("1.0.1")));
    }

    #[test]
    fn a_stale_generation_is_refused() {
        // The lost-update case, reduced to its essentials. Both writers read
        // generation 0; the second must not be allowed to publish work computed
        // from state that has already moved.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StateStore::open(dir.path()).expect("open");
        let current = store.initialise(namespace()).expect("initialise");

        let mut promoted = current.state.clone();
        promoted.active = Some(entry("1.0.1"));
        store
            .compare_and_set(current.generation, &promoted)
            .expect("first writer wins");

        let mut staged = current.state.clone(); // computed from generation 0
        staged.pending = Some(entry("1.0.2"));
        assert!(
            matches!(
                store.compare_and_set(current.generation, &staged),
                Err(CasError::Conflict)
            ),
            "a writer holding a stale generation must be refused"
        );

        // And the promotion survived, which is the property that matters.
        let reread = store.load().expect("load").expect("state");
        assert_eq!(reread.state.active, Some(entry("1.0.1")));
    }

    #[test]
    fn a_conflicted_writer_succeeds_after_re_reading() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StateStore::open(dir.path()).expect("open");
        let current = store.initialise(namespace()).expect("initialise");

        let mut promoted = current.state.clone();
        promoted.active = Some(entry("1.0.1"));
        store
            .compare_and_set(current.generation, &promoted)
            .expect("promotion");

        // Retry: re-read, recompute on top of what actually won, write again.
        let fresh = store.load().expect("load").expect("state");
        let mut staged = fresh.state.clone();
        staged.pending = Some(entry("1.0.2"));
        store
            .compare_and_set(fresh.generation, &staged)
            .expect("retry after re-read");

        let reread = store.load().expect("load").expect("state").state;
        assert_eq!(
            reread.active,
            Some(entry("1.0.1")),
            "the promotion must not have been reverted by the retry"
        );
        assert_eq!(reread.pending, Some(entry("1.0.2")));
    }

    #[test]
    fn concurrent_writers_never_lose_a_committed_transition() {
        // The interleaving argument, run rather than reasoned about. Many
        // threads race to append transitions; every successful CAS must be
        // visible in the final chain, and no committed value may be reverted.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("temp dir");
        let store = Arc::new(StateStore::open(dir.path()).expect("open"));
        store.initialise(namespace()).expect("initialise");

        let committed = Arc::new(AtomicUsize::new(0));
        let conflicts = Arc::new(AtomicUsize::new(0));

        std::thread::scope(|scope| {
            for worker in 0..8 {
                let store = Arc::clone(&store);
                let committed = Arc::clone(&committed);
                let conflicts = Arc::clone(&conflicts);
                scope.spawn(move || {
                    for round in 0..10 {
                        // Read-modify-write with retry, exactly as a real
                        // transition does.
                        loop {
                            let current = store.load().expect("load").expect("state");
                            let mut next = current.state.clone();
                            next.pending = Some(entry(&format!("{worker}.{round}.0")));
                            match store.compare_and_set(current.generation, &next) {
                                Ok(_) => {
                                    committed.fetch_add(1, Ordering::Relaxed);
                                    break;
                                }
                                Err(CasError::Conflict) => {
                                    conflicts.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(other) => panic!("unexpected: {other:?}"),
                            }
                        }
                    }
                });
            }
        });

        let commits = committed.load(Ordering::Relaxed);
        assert_eq!(commits, 80, "every worker round must eventually commit");

        // Generation count is the proof there were no lost updates: each commit
        // created exactly one new generation, so the final generation equals the
        // number of commits. A lost update would show up as a missing one.
        let final_state = store.load().expect("load").expect("state");
        assert_eq!(
            final_state.generation, commits as u64,
            "generations must equal commits, or a write was silently overwritten"
        );
        assert!(
            conflicts.load(Ordering::Relaxed) > 0,
            "the test is vacuous unless writers actually raced"
        );
    }

    #[test]
    fn pruning_keeps_the_current_state_readable() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StateStore::open(dir.path()).expect("open");
        let mut current = store.initialise(namespace()).expect("initialise");
        for i in 0..10 {
            let mut next = current.state.clone();
            next.pending = Some(entry(&format!("1.0.{i}")));
            let generation = store
                .compare_and_set(current.generation, &next)
                .expect("write");
            current = Versioned {
                generation,
                state: next,
            };
        }

        store.prune(2).expect("prune");
        let reread = store.load().expect("load").expect("state");
        assert_eq!(reread.generation, current.generation);
        assert_eq!(reread.state.pending, current.state.pending);
    }

    #[test]
    fn a_foreign_format_version_reads_as_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StateStore::open(dir.path()).expect("open");
        let mut state = CacheState::empty(namespace());
        state.format_version = CACHE_FORMAT_VERSION + 1;
        std::fs::write(
            dir.path().join("state.0.json"),
            serde_json::to_string(&state).expect("serialize"),
        )
        .expect("write");

        assert!(
            store.load().expect("load").is_none(),
            "a cache written by a future format must not be interpreted"
        );
    }
}
