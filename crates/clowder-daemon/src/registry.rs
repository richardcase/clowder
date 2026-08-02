use anyhow::Result;
use clowder_proto::PaneTree;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent_id: u64,
    pub project: PathBuf,
    pub task: String,
    pub adapter_id: String,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub workspace_kind: String,
    pub cols: u16,
    pub rows: u16,
    /// The agent's split layout at last change; `None` = a single agent leaf (also how M9a
    /// records — written before this field existed — deserialize). Rebuilt on reconcile (M9b).
    #[serde(default)]
    pub tree: Option<PaneTree>,
}

/// Durable, restart-surviving list of live agents. All state is in one JSON file written atomically.
pub struct Registry {
    path: PathBuf,
    /// Serializes the load-modify-write in `upsert`/`remove`. The daemon is the sole writer, but its
    /// control handlers run as concurrent Tokio tasks (app, CLI, remote client), so two unsynchronized
    /// `load()`-append-`write()` cycles would race and drop one update. One `Arc<Registry>` is shared,
    /// so this in-process mutex is the whole story (a single-instance flock guarantees one daemon).
    write_lock: Mutex<()>,
}

impl Registry {
    pub fn new(path: PathBuf) -> Self {
        Self { path, write_lock: Mutex::new(()) }
    }

    /// `$CLOWDER_STATE_FILE` › `$XDG_STATE_HOME/clowder/agents.json` › `$HOME/.local/state/clowder/agents.json`.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("CLOWDER_STATE_FILE") {
            if !p.is_empty() { return PathBuf::from(p); }
        }
        let base = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/state")))
            .unwrap_or_else(|| "/tmp".to_string());
        PathBuf::from(base).join("clowder").join("agents.json")
    }

    pub fn load(&self) -> Vec<AgentRecord> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("agent registry {} is unreadable ({e}); starting empty", self.path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(), // missing = empty
        }
    }

    pub fn upsert(&self, rec: AgentRecord) {
        // Hold the lock across the whole load-modify-write; recover a poisoned lock rather than
        // wedging the daemon (load/write never panic, so poisoning is not expected).
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        all.retain(|r| r.agent_id != rec.agent_id);
        all.push(rec);
        self.write(&all);
    }

    pub fn remove(&self, agent_id: u64) {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        all.retain(|r| r.agent_id != agent_id);
        self.write(&all);
    }

    /// Update just one agent's persisted split tree (no-op if the agent isn't in the registry —
    /// e.g. it was landed between a tree change and this call). Atomic, under `write_lock`.
    pub fn set_tree(&self, agent_id: u64, tree: Option<PaneTree>) {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        if let Some(rec) = all.iter_mut().find(|r| r.agent_id == agent_id) {
            rec.tree = tree;
            self.write(&all);
        }
    }

    fn write(&self, all: &[AgentRecord]) {
        if let Err(e) = self.try_write(all) {
            tracing::warn!("failed to persist agent registry {}: {e}", self.path.display());
        }
    }

    fn try_write(&self, all: &[AgentRecord]) -> Result<()> {
        if let Some(dir) = self.path.parent() { std::fs::create_dir_all(dir)?; }
        // Unique temp name (pid + counter) so a write never clobbers another writer's temp file
        // before its rename — belt-and-suspenders alongside `write_lock`.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = self.path.with_extension(format!(
            "json.{}.{}.tmp", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)?;
        std::fs::rename(&tmp, &self.path)?;   // atomic replace
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn rec(id: u64) -> AgentRecord {
        AgentRecord {
            agent_id: id, project: PathBuf::from("/p"), task: "t".into(),
            adapter_id: "claude".into(), worktree_path: PathBuf::from("/p/.clowder/worktrees/t"),
            branch: "clowder/t".into(), workspace_kind: "git".into(), cols: 80, rows: 24,
            tree: None,
        }
    }

    #[test]
    fn upsert_remove_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path().join("agents.json"));
        assert!(reg.load().is_empty());               // missing file → empty
        reg.upsert(rec(1));
        reg.upsert(rec(2));
        reg.upsert(AgentRecord { task: "t1b".into(), ..rec(1) });   // replace id 1
        let loaded = reg.load();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.iter().find(|r| r.agent_id == 1).unwrap().task, "t1b");
        reg.remove(1);
        assert_eq!(reg.load().iter().map(|r| r.agent_id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn concurrent_upserts_do_not_lose_records() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(Registry::new(dir.path().join("agents.json")));
        // Without the write lock, these racing load-append-write cycles drop updates (last writer
        // wins on the load() snapshot); with it, every id survives.
        let handles: Vec<_> = (0..16u64)
            .map(|i| { let reg = Arc::clone(&reg); std::thread::spawn(move || reg.upsert(rec(i))) })
            .collect();
        for h in handles { h.join().unwrap(); }
        let mut ids: Vec<u64> = reg.load().iter().map(|r| r.agent_id).collect();
        ids.sort();
        assert_eq!(ids, (0..16).collect::<Vec<_>>());
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("agents.json");
        std::fs::write(&p, b"not json").unwrap();
        assert!(Registry::new(p).load().is_empty());   // never panics
    }

    #[test]
    fn default_path_honors_env() {
        std::env::set_var("CLOWDER_STATE_FILE", "/tmp/x/agents.json");
        assert_eq!(Registry::default_path(), Path::new("/tmp/x/agents.json"));
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[test]
    fn record_with_tree_roundtrips() {
        use clowder_proto::{Axis, PaneId, PaneTree, SplitId};
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path().join("agents.json"));
        let tree = PaneTree::Split {
            id: SplitId(1), axis: Axis::Horizontal, ratio: 0.4,
            first: Box::new(PaneTree::Leaf { pane: PaneId(1) }),
            second: Box::new(PaneTree::Leaf { pane: PaneId(2) }),
        };
        reg.upsert(AgentRecord { tree: Some(tree.clone()), ..rec(1) });
        let loaded = reg.load();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].tree, Some(tree));
    }

    #[test]
    fn record_without_tree_key_defaults_to_none() {
        // A record written by M9a has no "tree" key; it must deserialize to None.
        let json = r#"[{"agent_id":1,"project":"/p","task":"t","adapter_id":"claude",
            "worktree_path":"/p/.clowder/worktrees/t","branch":"clowder/t",
            "workspace_kind":"git","cols":80,"rows":24}]"#;
        let recs: Vec<AgentRecord> = serde_json::from_str(json).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].tree, None);
    }

    #[test]
    fn set_tree_updates_one_record_and_noops_on_absent() {
        use clowder_proto::{PaneId, PaneTree};
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path().join("agents.json"));
        reg.upsert(rec(1));
        reg.upsert(rec(2));
        let t = PaneTree::Leaf { pane: PaneId(1) };
        reg.set_tree(1, Some(t.clone()));
        reg.set_tree(99, Some(PaneTree::Leaf { pane: PaneId(99) })); // absent → no-op, no panic
        let loaded = reg.load();
        assert_eq!(loaded.iter().find(|r| r.agent_id == 1).unwrap().tree, Some(t));
        assert_eq!(loaded.iter().find(|r| r.agent_id == 2).unwrap().tree, None);
        assert_eq!(loaded.len(), 2);
    }
}
