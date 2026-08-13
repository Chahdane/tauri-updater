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

/// How many times [`StateStore::initialise`] will step over a generation that
/// appeared underneath it before giving up.
///
/// Bounded rather than a `loop`: each iteration means another process published
/// a generation this build cannot read, and if that keeps happening the honest
/// answer is that the cache is unusable, not that we should keep trying.
const INITIALISE_ATTEMPTS: usize = 8;

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
    ///
    /// Lets a manifest's declared base tar be rejected without decompressing
    /// anything. It is a filter, not a proof — the tar is re-hashed after it is
    /// actually expanded.
    pub tar_blake3: String,
    /// Size of that tar.
    pub tar_size: u64,
    /// The base64 minisign signature published over the compressed artifact.
    ///
    /// Stored because nothing else can supply it later: the manifest for the
    /// *next* release carries a signature over the *next* artifact, so without
    /// this the cached base could never be re-verified. Keeping it here is what
    /// makes "the cache is untrusted on every reuse" implementable rather than
    /// aspirational.
    pub signature: String,
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

    /// Read the current state.
    ///
    /// `None` means there is no state this build can use — either the store is
    /// empty, or the newest generation is corrupt or written by a format this
    /// build does not implement. [`initialise`](StateStore::initialise) turns
    /// either into a fresh empty state.
    ///
    /// # Why the newest generation is the only candidate
    ///
    /// An earlier version scanned downwards and returned the newest generation
    /// it could *parse*, on the reasoning that falling back a generation beats
    /// refusing to read the cache at all. That is wrong, and it is wrong in the
    /// direction this whole module exists to prevent.
    ///
    /// Generation *N-1* is not an older description of the same state; it is a
    /// state that has been **superseded**. It may name a PENDING entry that has
    /// since been promoted, or an ACTIVE entry that has since been replaced.
    /// Reading it is exactly the lost update the compare-and-set refuses to
    /// allow a *writer* to cause — arrived at through the reader instead.
    ///
    /// It also deadlocks the store. A writer that read *N-1* may only publish
    /// *N*, which exists, so every attempt conflicts and no transition can ever
    /// commit again.
    ///
    /// So an unusable newest generation means the cache is unusable, which costs
    /// a full download and is the outcome every other cache failure has.
    pub fn load(&self) -> Result<Option<Versioned>> {
        let generations = self.generations()?;
        let Some(&generation) = generations.last() else {
            return Ok(None);
        };
        let Ok(text) = std::fs::read_to_string(self.path_for(generation)) else {
            return Ok(None);
        };
        match serde_json::from_str::<CacheState>(&text) {
            Ok(state) if state.format_version == CACHE_FORMAT_VERSION => {
                Ok(Some(Versioned { generation, state }))
            }
            _ => Ok(None),
        }
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
        self.publish(next, state).map(|()| next)
    }

    /// Return the current state, creating an empty one if there is none this
    /// build can use.
    ///
    /// The new state is published **above every generation present**, not at
    /// generation 0. Those are the same thing for an empty directory and very
    /// different for a directory holding a file this build cannot read: writing
    /// generation 0 there collides with a file that is not going away, and the
    /// store can never move again.
    pub fn initialise(&self, namespace: Namespace) -> Result<Versioned> {
        for _ in 0..INITIALISE_ATTEMPTS {
            if let Some(current) = self.load()? {
                return Ok(current);
            }
            // Nothing readable. Step over whatever is there rather than
            // assuming the store is empty.
            let next = self.generations()?.last().map_or(0, |g| g + 1);
            let state = CacheState::empty(namespace.clone());
            match self.publish(next, &state) {
                Ok(()) => {
                    return Ok(Versioned {
                        generation: next,
                        state,
                    })
                }
                // Someone else got there first. Re-read: theirs is as good as
                // ours, and if theirs is also unreadable this loop steps over
                // it too rather than giving up or panicking.
                Err(CasError::Conflict) => continue,
                Err(CasError::Failed(e)) => return Err(e),
            }
        }
        Err(Error::Manifest(
            "cache state could not be initialised; the store keeps changing under us".to_owned(),
        ))
    }

    /// Write `state` as generation `generation`, failing if it already exists.
    ///
    /// The whole compare-and-set is here: a complete file is written and synced
    /// under a unique temporary name, then linked to the generation name. The
    /// link is what fails if another writer got there first, and it is the only
    /// operation that makes a generation visible — so a generation name never
    /// refers to an incomplete file.
    fn publish(&self, generation: u64, state: &CacheState) -> std::result::Result<(), CasError> {
        let target = self.path_for(generation);
        let text = serde_json::to_string_pretty(state)
            .map_err(|e| CasError::Failed(Error::Manifest(e.to_string())))?;

        // Unique temp name so two racers never collide on the staging file.
        let temp = self.dir.join(format!(
            "state.{generation}.{}.{:?}.tmp",
            std::process::id(),
            std::thread::current().id()
        ));

        write_and_sync(&temp, text.as_bytes())
            .map_err(|e| CasError::Failed(Error::io("write", &temp, e)))?;

        // Fails with AlreadyExists on every supported platform if some other
        // writer already published this generation.
        let result = std::fs::hard_link(&temp, &target);
        let _ = std::fs::remove_file(&temp);

        match result {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(CasError::Conflict),
            Err(e) => Err(CasError::Failed(Error::io("link", &target, e))),
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
            signature: format!("signature-for-{version}"),
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

    // ---- regressions ------------------------------------------------------
    //
    // Both of these were live defects, found by the cache state machine built
    // on top of this store rather than by the store's own suite. They are the
    // same mistake in two places: treating "cannot read the newest generation"
    // as "there is no newest generation".

    #[test]
    fn a_store_whose_only_generation_is_unreadable_initialises_above_it() {
        // Previously panicked. `load` returned None, so `initialise` tried to
        // create generation 0, the link failed with AlreadyExists because the
        // file is right there, and the recovery path called `.expect()` on a
        // second `load` that returned None for the same reason.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StateStore::open(dir.path()).expect("open");
        let mut future = CacheState::empty(namespace());
        future.format_version = CACHE_FORMAT_VERSION + 1;
        std::fs::write(
            dir.path().join("state.0.json"),
            serde_json::to_string(&future).expect("serialize"),
        )
        .expect("write");

        let current = store
            .initialise(namespace())
            .expect("a cache from a future format must be stepped over, not fatal");
        assert_eq!(
            current.generation, 1,
            "the empty state must be published above the file that is there"
        );
        assert!(current.state.active.is_none());

        // And the store works from then on.
        let mut next = current.state.clone();
        next.pending = Some(entry("1.0.1"));
        store
            .compare_and_set(current.generation, &next)
            .expect("the store must be usable after recovery");
        assert_eq!(
            store.load().expect("load").expect("state").state.pending,
            Some(entry("1.0.1"))
        );
    }

    #[test]
    fn a_superseded_generation_is_never_read_in_place_of_the_newest() {
        // Previously, `load` scanned downwards for the newest generation it
        // could parse. That resurrects state the CAS had already replaced --
        // here, a PENDING entry that was promoted two generations ago -- and it
        // wedges the store, because a writer holding N-1 may only publish N,
        // which exists.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StateStore::open(dir.path()).expect("open");
        let current = store.initialise(namespace()).expect("initialise");

        let mut staged = current.state.clone();
        staged.pending = Some(entry("1.0.1"));
        let generation = store
            .compare_and_set(current.generation, &staged)
            .expect("stage");

        let mut promoted = staged.clone();
        promoted.active = Some(entry("1.0.1"));
        promoted.pending = None;
        let generation = store
            .compare_and_set(generation, &promoted)
            .expect("promote");

        // Corrupt the newest generation only.
        std::fs::write(store.path_for(generation), b"{ not json").expect("corrupt");

        assert!(
            store.load().expect("load").is_none(),
            "a superseded generation must not stand in for an unreadable newest one"
        );

        // Recovery empties rather than reverting: the alternative is offering a
        // PENDING that was promoted, as if it had not been.
        let recovered = store.initialise(namespace()).expect("initialise");
        assert_eq!(recovered.generation, generation + 1);
        assert!(recovered.state.active.is_none());
        assert!(recovered.state.pending.is_none());
    }
}
