# clowder M9a — Agent survival (persist + reconcile + resume) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A running agent survives a daemon restart: the daemon writes a durable registry of live agents and, on startup, re-spawns each one in its existing worktree using the adapter's resume (Claude `--continue`).

**Architecture:** A JSON `agents.json` registry (durable per-user dir). `spawn_agent` upserts a record; `finish_agent` (land/discard) removes it. On startup the daemon **reconciles** — for each record, verify the worktree exists (else prune), rebuild the adapter + workspace, and re-spawn with the adapter's resume launch under the **original** agent id. Splits reset to a single pane; scrollback isn't preserved (M9c). No wire-protocol or macOS-app change — the app already re-issues `listAgents` on reconnect.

**Tech Stack:** Rust (edition 2021), serde/serde_json, the existing `AgentAdapter`/`WorkspaceDriver`/`Pane` machinery.

## Global Constraints

- **Prefix cargo with `source "$HOME/.cargo/env" && `**; CI runs `cargo test --workspace --locked`.
- **Registry is durable, not ephemeral.** Path: `$CLOWDER_STATE_FILE` › `$XDG_STATE_HOME/clowder/agents.json` › `$HOME/.local/state/clowder/agents.json`. **Never** the `$TMPDIR`-based runtime/socket dir.
- **Never crash the daemon** on a missing/corrupt registry, missing worktree, or failed adapter/resume — prune + log and continue.
- **Original ids are stable:** reconcile re-registers each agent under its saved `agent_id` and sets the daemon's `next_id` above the max restored id so new spawns never collide.
- Reuse verbatim: `AgentAdapter` (`agent.rs`), `build_adapter(id) -> Option<Box<dyn AgentAdapter>>` (`agent.rs:153`), `Workspace {path,branch,project,kind}` + `driver_for_kind(kind)` (`clowder-workspace`), `Pane::spawn`, and the registration tail of `spawn_agent` (`server.rs:163-216`).
- **Resume-all-on-startup** (per spec Q2): reconcile re-spawns every registry entry; land/discard is how an agent is removed.

---

## Task 1: `WorkspaceKind` string mapping (clowder-workspace)

**Files:** Modify `crates/clowder-workspace/src/lib.rs`.

**Interfaces:** Produces `WorkspaceKind::as_str(&self) -> &'static str` (`"git"`/`"jj"`) and `WorkspaceKind::from_str(&str) -> Option<WorkspaceKind>`, so the registry can store the kind without a serde dependency on this crate.

- [ ] **Step 1: Failing test.** In `crates/clowder-workspace/src/lib.rs` `#[cfg(test)] mod tests`:
```rust
#[test]
fn workspace_kind_string_roundtrip() {
    for k in [WorkspaceKind::Git, WorkspaceKind::Jj] {
        assert_eq!(WorkspaceKind::from_str(k.as_str()), Some(k));
    }
    assert_eq!(WorkspaceKind::from_str("nope"), None);
}
```
- [ ] **Step 2: Run → fail.** `source "$HOME/.cargo/env" && cargo test -p clowder-workspace workspace_kind 2>&1 | tail -15` (no method `as_str`).
- [ ] **Step 3: Implement.** Ensure `WorkspaceKind` derives `PartialEq, Eq, Clone, Copy` (add any missing), and add:
```rust
impl WorkspaceKind {
    pub fn as_str(&self) -> &'static str {
        match self { WorkspaceKind::Git => "git", WorkspaceKind::Jj => "jj" }
    }
    pub fn from_str(s: &str) -> Option<WorkspaceKind> {
        match s { "git" => Some(WorkspaceKind::Git), "jj" => Some(WorkspaceKind::Jj), _ => None }
    }
}
```
- [ ] **Step 4: Run → pass.** `cargo test -p clowder-workspace 2>&1 | tail -15`.
- [ ] **Step 5: Commit** `feat(workspace): WorkspaceKind as_str/from_str for the agent registry`.

---

## Task 2: Agent registry module (clowder-daemon)

**Files:** Create `crates/clowder-daemon/src/registry.rs`; modify `crates/clowder-daemon/src/lib.rs` (`pub mod registry;`).

**Interfaces:**
- `pub struct AgentRecord { pub agent_id: u64, pub project: PathBuf, pub task: String, pub adapter_id: String, pub worktree_path: PathBuf, pub branch: String, pub workspace_kind: String, pub cols: u16, pub rows: u16 }` (derives `Serialize, Deserialize, Clone`).
- `pub struct Registry { path: PathBuf }` with `pub fn new(path: PathBuf) -> Self`, `pub fn default_path() -> PathBuf`, `pub fn load(&self) -> Vec<AgentRecord>` (empty on missing/corrupt), `pub fn upsert(&self, rec: AgentRecord)` (replace by `agent_id`), `pub fn remove(&self, agent_id: u64)`. Writes are atomic (temp file in the same dir + `rename`).

- [ ] **Step 1: Failing test.** Create `crates/clowder-daemon/src/registry.rs`:
```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
```
- [ ] **Step 2: Run → fail.** `source "$HOME/.cargo/env" && cargo test -p clowder-daemon registry 2>&1 | tail -20`.
- [ ] **Step 3: Implement** above the tests:
```rust
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
```
Add `pub mod registry;` to `crates/clowder-daemon/src/lib.rs`.
- [ ] **Step 4: Run → pass.** `cargo test -p clowder-daemon registry 2>&1 | tail -20`.
- [ ] **Step 5: Commit** `feat(daemon): durable agent registry (agents.json, atomic writes)`.

---

## Task 3: `resume_command` on the adapter trait

**Files:** Modify `crates/clowder-daemon/src/agent.rs`.

**Interfaces:** Adds `AgentAdapter::resume_command(&self, worktree: &Path) -> PaneCommand` with a default that calls `launch_command`; `ClaudeAdapter` overrides it.

- [ ] **Step 1: Failing test.** In `agent.rs` `#[cfg(test)] mod tests` (or add one):
```rust
#[test]
fn claude_resume_uses_continue_and_default_is_fresh() {
    let c = ClaudeAdapter;
    assert!(c.resume_command(std::path::Path::new("/w")).args.iter().any(|a| a == "--continue"));
    // an adapter without an override resumes exactly as it launches
    let s = CodexAdapter;
    assert_eq!(s.resume_command(std::path::Path::new("/w")).args, s.launch_command(std::path::Path::new("/w")).args);
}
```
- [ ] **Step 2: Run → fail.** `source "$HOME/.cargo/env" && cargo test -p clowder-daemon claude_resume 2>&1 | tail -15`.
- [ ] **Step 3: Implement.** Add to the trait (after `launch_command`):
```rust
    /// The command to RESUME an existing agent in its worktree on a daemon-restart reconcile.
    /// Defaults to a fresh launch; adapters override to continue a prior session.
    fn resume_command(&self, worktree: &Path) -> PaneCommand {
        self.launch_command(worktree)
    }
```
Override in `ClaudeAdapter`:
```rust
    fn resume_command(&self, _worktree: &Path) -> PaneCommand {
        // `claude --continue` resumes the most recent conversation in this worktree.
        PaneCommand { program: "claude".into(), args: vec!["--continue".into()], cwd: None, env: vec![] }
    }
```
- [ ] **Step 4: Run → pass.** `cargo test -p clowder-daemon claude_resume 2>&1 | tail -15`.
- [ ] **Step 5: Commit** `feat(daemon): AgentAdapter::resume_command (claude --continue) for reconcile`.

---

## Task 4: Retain `adapter_id`; persist on spawn + finish

**Files:** Modify `crates/clowder-daemon/src/server.rs`.

**Interfaces:** `AgentMeta` gains `pub adapter_id: String`. The `Daemon` gains a `registry: Arc<Registry>` field (default `Registry::default_path()`, overridable via a test setter). `spawn_agent` upserts a record; `finish_agent` removes it.

- [ ] **Step 1: Failing test.** Add to `server.rs` tests (model on the existing daemon tests — build a temp git repo + `Daemon::new_with`, point the registry at a temp file via a setter/env):
```rust
#[tokio::test]
async fn spawn_writes_registry_and_finish_removes_it() {
    let repo = tempfile::tempdir().unwrap();
    // init a git repo (see existing helpers)
    for a in [["init","-q"],["config","user.email","t@t"],["config","user.name","t"]] {
        assert!(std::process::Command::new("git").arg("-C").arg(repo.path()).args(a).status().unwrap().success());
    }
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    for a in [vec!["add","."], vec!["commit","-qm","i"]] {
        assert!(std::process::Command::new("git").arg("-C").arg(repo.path()).args(&a).status().unwrap().success());
    }
    let statedir = tempfile::tempdir().unwrap();
    std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));
    let d = std::sync::Arc::new(Daemon::new_with(std::sync::Arc::new(crate::FakeNotifier::new()), "/tmp/unused-m9.sock".into()));
    let adapter = crate::agent::SyntheticAdapter { command: /* a benign long-lived cmd, e.g. sleep */ PaneCommand { program: "sleep".into(), args: vec!["30".into()], cwd: None, env: vec![] } };
    let id = d.spawn_agent(repo.path(), &adapter, "demo").unwrap();
    let recs = crate::registry::Registry::new(statedir.path().join("agents.json")).load();
    assert_eq!(recs.iter().filter(|r| r.agent_id == id.0).count(), 1);
    assert_eq!(recs[0].adapter_id, "synthetic");
    d.discard_agent(id).unwrap();
    assert!(crate::registry::Registry::new(statedir.path().join("agents.json")).load().is_empty());
    std::env::remove_var("CLOWDER_STATE_FILE");
}
```
(Adjust to the exact `SyntheticAdapter`/`FakeNotifier`/`discard_agent` names in the crate.)
- [ ] **Step 2: Run → fail.** `cargo test -p clowder-daemon spawn_writes_registry 2>&1 | tail -25`.
- [ ] **Step 3: Implement.**
  - Add `pub adapter_id: String` to `AgentMeta`.
  - Add `registry: Arc<crate::registry::Registry>` to `Daemon`; initialize in `new_with` to `Arc::new(Registry::new(Registry::default_path()))`. (`default_path` reads `CLOWDER_STATE_FILE` at construction, so the test's env var is honored.)
  - In `spawn_agent`, after the workspace/agents inserts (server.rs:164-172), capture the `Workspace` fields and write the record:
    ```rust
    self.registry.upsert(crate::registry::AgentRecord {
        agent_id: id.0,
        project: ws.project.clone(),
        task: task.to_string(),
        adapter_id: adapter.id().to_string(),
        worktree_path: ws.path.clone(),
        branch: ws.branch.clone(),
        workspace_kind: ws.kind.as_str().to_string(),
        cols: self.default_cols,
        rows: self.default_rows,
    });
    ```
    (Capture `ws` fields before `self.workspaces.lock().insert(id, ws)` moves it, or clone.)
  - Store `adapter_id: adapter.id().to_string()` in the `AgentMeta` insert.
  - In `finish_agent`, after finalizing the workspace, add `self.registry.remove(pane.0);`.
- [ ] **Step 4: Run → pass.** `cargo test -p clowder-daemon 2>&1 | tail -20` (new test + existing green).
- [ ] **Step 5: Commit** `feat(daemon): persist agents to the registry on spawn; remove on land/discard`.

---

## Task 5: Reconcile on startup

**Files:** Modify `crates/clowder-daemon/src/server.rs` (extract a registration helper + add `reconcile`); modify `crates/clowder-daemon/src/main.rs` (call it before serving).

**Interfaces:** `pub fn Daemon::reconcile(self: &Arc<Self>)` — load the registry, prune records whose worktree is gone, re-spawn the rest under their original ids with the adapter's resume, and bump `next_id`.

- [ ] **Step 1: Refactor (no behavior change).** Extract the registration tail of `spawn_agent` (register_pane + workspaces + agents + attention + hookless scanner + tree + wait_exit watcher, `server.rs:163-216`) into a private helper `fn finalize_agent(self: &Arc<Self>, id: PaneId, pane: Pane, ws: Workspace, task: &str, adapter: &dyn AgentAdapter)` and call it from `spawn_agent`. Run `cargo test -p clowder-daemon 2>&1 | tail -8` → still green (pure refactor).
- [ ] **Step 2: Failing test.**
```rust
#[tokio::test]
async fn reconcile_respawns_recorded_agents_and_prunes_missing() {
    // Spawn an agent (as in Task 4) so a worktree + registry record exist, then simulate a fresh
    // daemon: build a NEW Daemon pointed at the same CLOWDER_STATE_FILE and call reconcile().
    // ... (init git repo + CLOWDER_STATE_FILE as in Task 4; spawn via a first daemon) ...
    let d2 = std::sync::Arc::new(Daemon::new_with(std::sync::Arc::new(crate::FakeNotifier::new()), "/tmp/unused-m9b.sock".into()));
    d2.reconcile();
    assert_eq!(d2.list_agents().len(), 1);                    // re-registered under the original id
    // Now corrupt: remove the worktree dir and reconcile a third daemon → pruned.
    std::fs::remove_dir_all(&worktree_path).unwrap();
    let d3 = std::sync::Arc::new(Daemon::new_with(...));
    d3.reconcile();
    assert!(d3.list_agents().is_empty());
    assert!(crate::registry::Registry::new(state_path).load().is_empty());  // pruned from disk too
}
```
(Use `list_agents()` — the existing accessor behind `ControlRequest::ListAgents`.)
- [ ] **Step 3: Run → fail.** `cargo test -p clowder-daemon reconcile_respawns 2>&1 | tail -25`.
- [ ] **Step 4: Implement `reconcile`:**
```rust
/// Re-spawn every agent recorded in the registry (agents survive a daemon restart). Prunes records
/// whose worktree is gone or whose adapter/resume fails; never panics.
pub fn reconcile(self: &Arc<Self>) {
    let records = self.registry.load();
    let mut max_id = 0u64;
    for rec in records {
        max_id = max_id.max(rec.agent_id);
        let id = PaneId(rec.agent_id);
        if !rec.worktree_path.exists() {
            tracing::warn!("agent {} worktree {} is gone; pruning", rec.agent_id, rec.worktree_path.display());
            self.registry.remove(rec.agent_id);
            continue;
        }
        let Some(kind) = clowder_workspace::WorkspaceKind::from_str(&rec.workspace_kind) else {
            self.registry.remove(rec.agent_id); continue;
        };
        let Some(adapter) = crate::agent::build_adapter(&rec.adapter_id) else {
            self.registry.remove(rec.agent_id); continue;
        };
        let ws = Workspace { path: rec.worktree_path.clone(), branch: rec.branch.clone(),
                             project: rec.project.clone(), kind };
        let spawn = (|| -> anyhow::Result<Pane> {
            adapter.provision_hooks(&ws.path, id, &self.hook_sock)?;
            let mut cmd = adapter.resume_command(&ws.path);
            cmd.cwd = Some(ws.path.clone());
            cmd.env.push(("CLOWDER_AGENT_ID".into(), id.0.to_string()));
            cmd.env.push(("CLOWDER_HOOK_SOCK".into(), self.hook_sock.to_string_lossy().to_string()));
            Pane::spawn(id, cmd, rec.cols, rec.rows, self.backlog_cap)
        })();
        match spawn {
            Ok(pane) => self.finalize_agent(id, pane, ws, &rec.task, adapter.as_ref()),
            Err(e) => { tracing::warn!("resume agent {} failed: {e}; pruning", rec.agent_id); self.registry.remove(rec.agent_id); }
        }
    }
    // New spawns must not collide with restored ids.
    self.bump_next_id_above(max_id);
}
```
Add a small `fn bump_next_id_above(&self, n: u64)` that sets `next_id` to `max(current, n + 1)` (compare-and-store on the `AtomicU64`). `finalize_agent` also writes the registry via `spawn_agent`? No — the record already exists; `finalize_agent` must NOT upsert (keep the upsert in `spawn_agent` only, not the shared helper).
- [ ] **Step 5: Wire into `main.rs`.** After `let daemon = Arc::new(Daemon::new_from_config(config));` and before the serve `select!` (after binding sockets), add `daemon.reconcile();` with a log line.
- [ ] **Step 6: Run → pass + full suite.** `cargo test -p clowder-daemon reconcile_respawns 2>&1 | tail -20` then `cargo test --workspace --locked 2>&1 | tail -12`.
- [ ] **Step 7: Manual smoke.** `source "$HOME/.cargo/env" && cargo build`; in one shell run the daemon with a temp state file + a real repo, `clowder spawn <repo> demo shell`; `kill` the daemon; restart it (same `CLOWDER_STATE_FILE`) → the shell agent reappears (`clowder`… `ListAgents` via the control socket shows it) in the same worktree; `clowder land`/`discard` then restart → it does not come back.
- [ ] **Step 8: Commit** `feat(daemon): reconcile — re-spawn persisted agents with resume on startup`.

---

## Self-Review

- **Spec coverage (M9a):** durable registry (Task 2) ✓; retain adapter_id + persist on spawn/finish (Task 4) ✓; reconcile-on-startup re-spawns live agents in their worktrees under original ids, prunes stale (Task 5) ✓; adapter resume / Claude `--continue` (Task 3) ✓; never-crash on bad registry/worktree/adapter (Tasks 2, 5) ✓; splits reset (finalize_agent inserts a single-leaf tree — unchanged) ✓. M9b (layout) + M9c (PTY-host) are out of scope.
- **Placeholders:** the two integration tests (Tasks 4–5) sketch the git-repo/env setup and say "adjust to exact names" — the implementer must fill exact `SyntheticAdapter`/`FakeNotifier`/`discard_agent`/`list_agents` names from the crate; all other steps are concrete.
- **Type consistency:** `AgentRecord`/`Registry` (Task 2) consumed by Tasks 4–5; `WorkspaceKind::as_str`/`from_str` (Task 1) used in Tasks 4–5; `resume_command` (Task 3) used in Task 5; `finalize_agent` (Task 5 refactor) shared by `spawn_agent` + `reconcile`.
