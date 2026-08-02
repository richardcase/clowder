use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
}

/// Durable, restart-surviving list of live agents. All state is in one JSON file written atomically.
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
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
        let mut all = self.load();
        all.retain(|r| r.agent_id != rec.agent_id);
        all.push(rec);
        self.write(&all);
    }

    pub fn remove(&self, agent_id: u64) {
        let mut all = self.load();
        all.retain(|r| r.agent_id != agent_id);
        self.write(&all);
    }

    fn write(&self, all: &[AgentRecord]) {
        if let Err(e) = self.try_write(all) {
            tracing::warn!("failed to persist agent registry {}: {e}", self.path.display());
        }
    }

    fn try_write(&self, all: &[AgentRecord]) -> Result<()> {
        if let Some(dir) = self.path.parent() { std::fs::create_dir_all(dir)?; }
        let tmp = self.path.with_extension("json.tmp");
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
}
