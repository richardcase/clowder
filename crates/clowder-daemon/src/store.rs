// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// A durable `Vec<T>` held in one JSON file, written atomically.
///
/// All mutation goes through `mutate` or `mutate_if`, both of which hold `write_lock` across
/// the whole load-modify-write. The daemon is the sole writer, but its control handlers run as
/// concurrent Tokio tasks (app, CLI, remote client), so two unsynchronized load-append-write
/// cycles would race and drop one update. One `Arc<JsonStore>` is shared per file, and a
/// single-instance flock guarantees one daemon, so this in-process mutex is the whole story.
pub struct JsonStore<T> {
    path: PathBuf,
    write_lock: Mutex<()>,
    /// `fn() -> T` rather than `T` so the store is `Send + Sync` regardless of `T`.
    _marker: PhantomData<fn() -> T>,
}

impl<T: Serialize + DeserializeOwned> JsonStore<T> {
    pub fn new(path: PathBuf) -> Self {
        Self { path, write_lock: Mutex::new(()), _marker: PhantomData }
    }

    /// The current contents. A missing file is empty; an unreadable one warns and is empty.
    /// Never panics — a corrupt state file must not wedge the daemon.
    pub fn load(&self) -> Vec<T> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("state file {} is unreadable ({e}); starting empty", self.path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(), // missing = empty
        }
    }

    /// Load, apply `f`, write back — all under one lock. Returns `f`'s value.
    pub fn mutate<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R) -> R {
        // Recover a poisoned lock rather than wedging the daemon (load/write never panic,
        // so poisoning is not expected).
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        let out = f(&mut all);
        if let Err(e) = self.try_write(&all) {
            tracing::warn!("failed to persist state file {}: {e}", self.path.display());
        }
        out
    }

    /// Like `mutate`, but returns the write error instead of only logging it. Use this for
    /// operations answering a user request, where silently failing to persist would report
    /// success for something that never reached disk.
    pub fn try_mutate<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R) -> Result<R> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        let out = f(&mut all);
        self.try_write(&all)?;
        Ok(out)
    }

    /// Like `mutate`, but skips the write when `f` returns false — so a caller that
    /// finds nothing to change costs no I/O.
    pub fn mutate_if(&self, f: impl FnOnce(&mut Vec<T>) -> bool) {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        if !f(&mut all) {
            return;
        }
        if let Err(e) = self.try_write(&all) {
            tracing::warn!("failed to persist state file {}: {e}", self.path.display());
        }
    }

    fn try_write(&self, all: &[T]) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Unique temp name (pid + counter) so a write never clobbers another writer's temp
        // file before its rename — belt-and-suspenders alongside `write_lock`.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = self.path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)?;
        std::fs::rename(&tmp, &self.path)?; // atomic replace
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Item { id: u64, label: String }

    #[test]
    fn missing_file_loads_empty_and_mutate_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("items.json");
        let store: JsonStore<Item> = JsonStore::new(p.clone());
        assert!(store.load().is_empty());
        store.mutate(|all| all.push(Item { id: 1, label: "a".into() }));
        assert!(p.exists(), "mutate must create the file and its parent dir");
        assert_eq!(store.load(), vec![Item { id: 1, label: "a".into() }]);
    }

    #[test]
    fn mutate_returns_the_closures_value() {
        let dir = tempfile::tempdir().unwrap();
        let store: JsonStore<Item> = JsonStore::new(dir.path().join("items.json"));
        store.mutate(|all| all.push(Item { id: 7, label: "x".into() }));
        let found = store.mutate(|all| all.iter().any(|i| i.id == 7));
        assert!(found);
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("items.json");
        std::fs::write(&p, b"not json").unwrap();
        let store: JsonStore<Item> = JsonStore::new(p);
        assert!(store.load().is_empty(), "must never panic on a corrupt file");
    }

    #[test]
    fn mutate_if_false_does_not_touch_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("items.json");
        let store: JsonStore<Item> = JsonStore::new(p.clone());
        // The closure finds nothing to change and returns false: mutate_if must not create
        // the file at all (a fresh path is an unambiguous signal — no mtime-granularity races).
        store.mutate_if(|_all| false);
        assert!(!p.exists(), "mutate_if must not write when the closure reports no change");
    }

    #[test]
    fn mutate_if_true_persists_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let store: JsonStore<Item> = JsonStore::new(dir.path().join("items.json"));
        store.mutate_if(|all| {
            all.push(Item { id: 1, label: "a".into() });
            true
        });
        assert_eq!(store.load(), vec![Item { id: 1, label: "a".into() }]);
    }

    #[test]
    fn concurrent_mutates_do_not_lose_records() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<JsonStore<Item>> = Arc::new(JsonStore::new(dir.path().join("items.json")));
        let handles: Vec<_> = (0..16u64)
            .map(|i| {
                let s = Arc::clone(&store);
                std::thread::spawn(move || s.mutate(|all| all.push(Item { id: i, label: "c".into() })))
            })
            .collect();
        for h in handles { h.join().unwrap(); }
        let mut ids: Vec<u64> = store.load().iter().map(|i| i.id).collect();
        ids.sort();
        assert_eq!(ids, (0..16).collect::<Vec<_>>());
    }

    #[test]
    fn try_mutate_surfaces_write_failures() {
        // Point the store at a path whose parent cannot be created (a FILE, not a dir),
        // so create_dir_all fails and the error must reach the caller.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let store: JsonStore<Item> = JsonStore::new(blocker.join("items.json"));
        let err = store.try_mutate(|all| all.push(Item { id: 1, label: "a".into() }));
        assert!(err.is_err(), "try_mutate must not silently swallow a write failure");
    }

    #[test]
    fn try_mutate_returns_value_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let store: JsonStore<Item> = JsonStore::new(dir.path().join("items.json"));
        let n = store.try_mutate(|all| { all.push(Item { id: 3, label: "x".into() }); all.len() }).unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.load().len(), 1);
    }
}
