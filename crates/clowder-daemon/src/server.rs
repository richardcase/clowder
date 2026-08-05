use crate::agent::AgentAdapter;
use crate::notify::{Notifier, OsNotifier};
use crate::{Pane, PaneCommand};
use anyhow::{bail, Context, Result};
use clowder_proto::AttentionState;
use clowder_proto::{ClientToDaemon, DaemonToClient, MsgStream, PaneId};
use clowder_proto::{PaneTree, SplitDirection, SplitId};
use clowder_workspace::{driver_for, driver_for_kind, Workspace};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;
use tokio::sync::broadcast;

struct AgentMeta {
    /// Full path to the project root.
    project: String,
    name: String,
    branch: String,
}

/// How often the coalesced layout flusher persists agents whose divider ratios changed.
const LAYOUT_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(750);

/// The command for a companion pane: the login shell, rooted in the worktree, with no hook env.
pub(crate) fn companion_command(shell: String, cwd: std::path::PathBuf) -> PaneCommand {
    PaneCommand { program: shell, args: vec![], cwd: Some(cwd), env: vec![] }
}

/// Wire form of a project record: display name derived from the path's last component.
pub(crate) fn project_info(rec: crate::projects::ProjectRecord) -> clowder_proto::ProjectInfo {
    let name = rec.path.file_name().map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rec.path.to_string_lossy().to_string());
    clowder_proto::ProjectInfo {
        path: rec.path.to_string_lossy().to_string(),
        name,
        kind: rec.kind,
    }
}

/// A change to the project list, broadcast to every connected client.
#[derive(Clone, Debug)]
pub enum ProjectChange {
    Added(crate::projects::ProjectRecord),
    Removed(PathBuf),
    TerminalClosed(PathBuf),
}

pub struct Daemon {
    panes: Arc<Mutex<HashMap<PaneId, Arc<Pane>>>>,
    next_id: AtomicU64,
    attention: Arc<Mutex<HashMap<PaneId, AttentionState>>>,
    attention_tx: broadcast::Sender<(PaneId, AttentionState)>,
    removed_tx: broadcast::Sender<PaneId>,
    workspaces: Arc<Mutex<HashMap<PaneId, Workspace>>>,
    agents: Arc<Mutex<HashMap<PaneId, AgentMeta>>>,
    watchers: Arc<Mutex<HashMap<PaneId, tokio::task::JoinHandle<()>>>>,
    notifier: Arc<dyn Notifier>,
    hook_sock: PathBuf,
    trees: Arc<Mutex<HashMap<PaneId, PaneTree>>>, // agent pane -> split tree
    owner: Arc<Mutex<HashMap<PaneId, PaneId>>>,   // any leaf pane -> its agent
    next_split_id: AtomicU64,
    split_tx: broadcast::Sender<(PaneId, PaneTree)>,
    hookless: Arc<Mutex<std::collections::HashSet<PaneId>>>,
    scanners: Arc<Mutex<HashMap<PaneId, tokio::task::JoinHandle<()>>>>,
    companion_watchers: Arc<Mutex<HashMap<PaneId, tokio::task::JoinHandle<()>>>>,
    pub(crate) backlog_cap: usize,
    pub(crate) default_cols: u16,
    pub(crate) default_rows: u16,
    pub(crate) shell: String,
    registry: Arc<crate::registry::Registry>,
    projects: Arc<crate::projects::ProjectStore>,
    projects_tx: broadcast::Sender<ProjectChange>,
    /// Project root -> its lazily-spawned terminal pane. Not persisted.
    project_terms: Arc<Mutex<HashMap<PathBuf, PaneId>>>,
    /// Terminal pane -> its project root. The inverse of `project_terms`.
    term_project: Arc<Mutex<HashMap<PaneId, PathBuf>>>,
    /// Agents whose ratios changed since the last flush; drained by the periodic layout flusher.
    layout_dirty: Arc<Mutex<std::collections::HashSet<PaneId>>>,
    /// Idle debounce before content-based attention inspects the screen for a blocking prompt.
    pub(crate) content_idle: std::time::Duration,
    /// Serializes project-list mutations against agent spawns. `spawn_agent` validates the project
    /// then provisions for hundreds of milliseconds before it appears in `agents`; without this,
    /// a concurrent `remove_project` counts zero worktrees and removes a project out from under a
    /// live agent. Held across the WHOLE of each operation, not just the check.
    project_mutation: Mutex<()>,
}

impl Daemon {
    pub fn new() -> Daemon {
        Daemon::new_with(Arc::new(OsNotifier), PathBuf::from("/tmp/clowder-hook.sock"))
    }

    pub fn new_with(notifier: Arc<dyn Notifier>, hook_sock: PathBuf) -> Daemon {
        Daemon::new_with_paths(
            notifier,
            hook_sock,
            crate::registry::Registry::default_path(),
            crate::projects::ProjectStore::default_path(),
        )
    }

    /// Like `new_with`, but with both state files given explicitly. Tests use this to point at a
    /// temp dir without setting process-global env vars (which would force them to serialize).
    pub fn new_with_paths(
        notifier: Arc<dyn Notifier>,
        hook_sock: PathBuf,
        registry_path: PathBuf,
        projects_path: PathBuf,
    ) -> Daemon {
        let (attention_tx, _) = broadcast::channel(256);
        let (removed_tx, _) = broadcast::channel(256);
        let (split_tx, _) = broadcast::channel(256);
        let (projects_tx, _) = broadcast::channel(256);
        Daemon {
            panes: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            attention: Arc::new(Mutex::new(HashMap::new())),
            attention_tx,
            removed_tx,
            workspaces: Arc::new(Mutex::new(HashMap::new())),
            agents: Arc::new(Mutex::new(HashMap::new())),
            watchers: Arc::new(Mutex::new(HashMap::new())),
            notifier,
            hook_sock,
            trees: Arc::new(Mutex::new(HashMap::new())),
            owner: Arc::new(Mutex::new(HashMap::new())),
            next_split_id: AtomicU64::new(1),
            split_tx,
            hookless: Arc::new(Mutex::new(std::collections::HashSet::new())),
            scanners: Arc::new(Mutex::new(HashMap::new())),
            companion_watchers: Arc::new(Mutex::new(HashMap::new())),
            backlog_cap: 256 * 1024,
            default_cols: 80,
            default_rows: 24,
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            registry: Arc::new(crate::registry::Registry::new(registry_path)),
            projects: Arc::new(crate::projects::ProjectStore::new(projects_path)),
            projects_tx,
            project_terms: Arc::new(Mutex::new(HashMap::new())),
            term_project: Arc::new(Mutex::new(HashMap::new())),
            layout_dirty: Arc::new(Mutex::new(std::collections::HashSet::new())),
            content_idle: std::time::Duration::from_millis(500),
            project_mutation: Mutex::new(()),
        }
    }

    /// Build a daemon whose pane defaults (sockets already resolved into `hook_sock`, backlog cap,
    /// shell, pane size) come from `clowder-config`. Uses `OsNotifier` like `new()`.
    pub fn new_from_config(config: clowder_config::Config) -> Daemon {
        let mut d = Daemon::new_with(Arc::new(OsNotifier), config.hook_sock);
        d.backlog_cap = config.backlog_cap;
        d.default_cols = config.default_cols;
        d.default_rows = config.default_rows;
        d.shell = config.shell;
        d
    }

    pub fn subscribe_projects(&self) -> broadcast::Receiver<ProjectChange> {
        self.projects_tx.subscribe()
    }

    pub fn list_projects(&self) -> Vec<clowder_proto::ProjectInfo> {
        let mut recs = self.projects.list();
        recs.sort_by(|a, b| a.path.cmp(&b.path));
        recs.into_iter().map(project_info).collect()
    }

    /// Is `path` (canonicalized here) a registered project?
    pub fn is_registered_project(&self, path: &Path) -> bool {
        match path.canonicalize() {
            Ok(c) => self.projects.contains(&c),
            Err(_) => false,
        }
    }

    pub fn add_project(&self, path: &Path) -> Result<crate::projects::ProjectRecord> {
        let rec = self.projects.add(path)?;
        let _ = self.projects_tx.send(ProjectChange::Added(rec.clone()));
        Ok(rec)
    }

    /// Remove a project. Refused while any worktree still belongs to it — there must be no path
    /// by which removing a sidebar row abandons live work.
    pub fn remove_project(&self, path: &Path) -> Result<()> {
        let _mutation = self.project_mutation.lock();
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        // Canonicalize BOTH sides. Task 5 makes spawn_agent store a canonical path, but this
        // must be correct before that lands too — otherwise on macOS an uncanonical
        // AgentMeta.project (/var/...) never matches a canonical project (/private/var/...),
        // the count comes back 0, and the guard silently lets the removal through.
        let n = self
            .agents
            .lock()
            .values()
            .filter(|m| {
                let p = Path::new(&m.project);
                p.canonicalize().unwrap_or_else(|_| p.to_path_buf()) == canonical
            })
            .count();
        if n > 0 {
            bail!("project {} still has {n} worktree(s) — land or discard them first", canonical.display());
        }
        // Kill this project's terminal (and its companions, via forget_project_terminal's
        // cascade) before dropping the record — a removed project must not leave an orphaned
        // shell behind. Bind the lookup out of the `if let` scrutinee: in edition 2021, a
        // MutexGuard temporary created there lives for the whole `if let` body, and the body
        // below re-locks `project_terms` (via `forget_project_terminal`) — left inline, that
        // self-deadlocks.
        let term = self.project_terms.lock().get(&canonical).copied();
        if let Some(term) = term {
            if let Some(p) = self.get(term) { let _ = p.kill(); }
            self.forget_project_terminal(term);
        }
        self.projects.remove(&canonical)?;
        let _ = self.projects_tx.send(ProjectChange::Removed(canonical));
        Ok(())
    }

    /// The shell pane rooted at a project. Lazy and idempotent: a second caller attaches to the
    /// same shell. Not persisted — a daemon restart drops it and the next select respawns.
    pub fn open_project_terminal(self: &Arc<Self>, path: &Path) -> Result<PaneId> {
        let root = path.canonicalize()
            .with_context(|| format!("no such project path: {}", path.display()))?;
        if !self.projects.contains(&root) {
            bail!("unknown project: {} — add it first", root.display());
        }
        // Bind out of the `if let` scrutinee (same reasoning as remove_project): the MutexGuard
        // temporary otherwise lives for the whole body, which doesn't deadlock today (`self.get`
        // locks `panes`, a different mutex) but is one refactor away from doing so.
        let existing = self.project_terms.lock().get(&root).copied();
        if let Some(existing) = existing {
            if self.get(existing).is_some() {
                return Ok(existing);
            }
        }
        let id = self.spawn_pane(
            companion_command(self.shell.clone(), root.clone()),
            self.default_cols,
            self.default_rows,
        )?;
        // Two callers can both observe `existing == None` above and both reach this spawn — the
        // `project_terms` guard was released before the (long) fork, so it does not serialize
        // spawns. Re-check now, under the guard, before publishing: if another racer already won
        // while we were spawning, kill our pane and hand back the winner instead of leaving ours
        // live-but-unreachable via `project_terms` (see the concurrent-open finding). `self.get`
        // and `self.panes.lock()` stay OUT from under the `project_terms` guard — dropped before
        // either — so this can't introduce a `project_terms -> panes` lock-ordering inversion.
        let mut pt = self.project_terms.lock();
        if let Some(&winner) = pt.get(&root) {
            drop(pt);
            if let Some(p) = self.get(id) { let _ = p.kill(); }
            self.panes.lock().remove(&id);
            return Ok(winner);
        }
        pt.insert(root.clone(), id);
        drop(pt);
        self.term_project.lock().insert(id, root.clone());
        // Seed exactly what finalize_agent seeds for an agent root, so the split/close/ratio
        // machinery — which is keyed on a root pane — applies unchanged. Only the winner reaches
        // here: seeding a loser's tree/owner would let the split machinery believe two panes are
        // simultaneously the project's terminal.
        self.trees.lock().insert(id, PaneTree::Leaf { pane: id });
        self.owner.lock().insert(id, id);

        // When the shell exits, forget it so the next select respawns.
        if let Some(pane_arc) = self.get(id) {
            let me = Arc::clone(self);
            let handle = tokio::spawn(async move {
                pane_arc.wait_exit().await;
                me.forget_project_terminal(id);
            });
            self.watchers.lock().insert(id, handle);
        }
        Ok(id)
    }

    pub fn project_of_terminal(&self, pane: PaneId) -> Option<PathBuf> {
        self.term_project.lock().get(&pane).cloned()
    }

    /// Drop all state for a project terminal whose pane is gone, and tell clients. Idempotent —
    /// a no-op if `pane` is not (or is no longer) a project terminal, so callers that already
    /// killed the pane and callers racing the pane's own exit watcher can both call this safely.
    ///
    /// Cascades to companions first (mirrors `finish_agent`'s agent-teardown cascade): a project
    /// terminal that was split must not orphan its companion panes when the root is forgotten,
    /// whether that happens via a natural exit, an explicit close, or the project being removed
    /// — every one of those three callers reaches this same cascade instead of each duplicating
    /// (and risking missing) it.
    pub(crate) fn forget_project_terminal(&self, pane: PaneId) {
        let Some(root) = self.term_project.lock().remove(&pane) else { return };
        // Only clear the forward map if it still points at this pane. A losing racer from
        // `open_project_terminal`'s spawn window (or a caller that already killed a stale pane)
        // must not delete a *different*, live winner's mapping out from under it.
        let mut pt = self.project_terms.lock();
        if pt.get(&root) == Some(&pane) {
            pt.remove(&root);
        }
        drop(pt);

        let companions: Vec<PaneId> = self.trees.lock().get(&pane)
            .map(|t| crate::split_tree::leaves(t).into_iter().filter(|p| *p != pane).collect())
            .unwrap_or_default();
        for c in &companions {
            if let Some(p) = self.get(*c) { let _ = p.kill(); }
            self.panes.lock().remove(c);
            self.owner.lock().remove(c);
            if let Some(h) = self.companion_watchers.lock().remove(c) { h.abort(); }
        }

        self.trees.lock().remove(&pane);
        self.owner.lock().remove(&pane);
        if let Some(p) = self.get(pane) { let _ = p.kill(); }
        self.panes.lock().remove(&pane);
        // The exit watcher may be calling this itself (natural exit) or a caller that already
        // killed the pane may not have touched `watchers` (e.g. close_pane) — either way, drop
        // the entry so it doesn't accumulate under a dead PaneId across respawns.
        if let Some(h) = self.watchers.lock().remove(&pane) {
            h.abort();
        }
        let _ = self.projects_tx.send(ProjectChange::TerminalClosed(root));
    }

    pub fn set_attention(&self, pane: PaneId, state: AttentionState) {
        self.attention.lock().insert(pane, state);
        let _ = self.attention_tx.send((pane, state));
        self.notifier.notify(pane, state);
    }

    pub fn attention_of(&self, pane: PaneId) -> Option<AttentionState> {
        self.attention.lock().get(&pane).copied()
    }

    pub fn subscribe_attention(&self) -> broadcast::Receiver<(PaneId, AttentionState)> {
        self.attention_tx.subscribe()
    }

    pub fn subscribe_removed(&self) -> broadcast::Receiver<PaneId> {
        self.removed_tx.subscribe()
    }

    /// Path the daemon injects into agents as CLOWDER_HOOK_SOCK.
    pub fn hook_sock(&self) -> &std::path::Path {
        &self.hook_sock
    }

    fn alloc_id(&self) -> PaneId {
        PaneId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Ensure future `alloc_id` calls never collide with an id already restored by `reconcile`.
    fn bump_next_id_above(&self, n: u64) {
        self.next_id.fetch_max(n + 1, Ordering::Relaxed);
    }

    /// Re-spawn every agent recorded in the registry (agents survive a daemon restart). Prunes
    /// records whose worktree is gone or whose adapter/resume fails; never panics.
    pub fn reconcile(self: &Arc<Self>) {
        let records = self.registry.load();
        // Bump BEFORE restoring: companion `alloc_id()`s during layout restore must not collide with
        // a not-yet-restored agent's fixed id. (Agents re-spawn under `PaneId(rec.agent_id)`.)
        let max_id = records.iter().map(|r| r.agent_id).max().unwrap_or(0);
        self.bump_next_id_above(max_id);
        for rec in records {
            if let Err(e) = self.resume_agent(&rec) {
                tracing::warn!("resume agent {} failed: {e}; pruning", rec.agent_id);
                self.registry.remove(rec.agent_id);
            }
        }
    }

    /// Re-spawn one recorded agent under its original pane id: provision hooks, run the adapter's
    /// resume command, finalize, restore its companion layout. Shared by `reconcile` (daemon
    /// restart) and `restart_worktree` (user request), so the two cannot drift apart.
    fn resume_agent(self: &Arc<Self>, rec: &crate::registry::AgentRecord) -> Result<PaneId> {
        let id = PaneId(rec.agent_id);
        if !rec.worktree_path.exists() {
            bail!("worktree {} is gone", rec.worktree_path.display());
        }
        let kind = clowder_workspace::WorkspaceKind::from_str(&rec.workspace_kind)
            .ok_or_else(|| anyhow::anyhow!("unknown workspace kind {:?}", rec.workspace_kind))?;
        let adapter = crate::agent::build_adapter(&rec.adapter_id)
            .ok_or_else(|| anyhow::anyhow!("unknown adapter {:?}", rec.adapter_id))?;
        let ws = Workspace {
            path: rec.worktree_path.clone(),
            branch: rec.branch.clone(),
            project: rec.project.clone(),
            kind,
        };
        adapter.provision_hooks(&ws.path, id, &self.hook_sock)?;
        let mut cmd = adapter.resume_command(&ws.path);
        cmd.cwd = Some(ws.path.clone());
        cmd.env.push(("CLOWDER_AGENT_ID".into(), id.0.to_string()));
        cmd.env.push(("CLOWDER_HOOK_SOCK".into(), self.hook_sock.to_string_lossy().to_string()));
        let pane = Pane::spawn(id, cmd, rec.cols, rec.rows, self.backlog_cap)?;
        let restore_cwd = ws.path.clone();
        self.finalize_agent(id, pane, ws, &rec.task, adapter.as_ref());
        if let Some(tree) = rec.tree.clone() {
            self.restore_layout(id, tree, restore_cwd);
        }
        Ok(id)
    }

    /// Re-run an exited agent in its existing worktree, keeping its pane id (the worktree's
    /// durable identity) and any live companion panes.
    pub fn restart_worktree(self: &Arc<Self>, pane: PaneId) -> Result<()> {
        match self.attention_of(pane) {
            None => bail!("no worktree with pane {}", pane.0),
            Some(AttentionState::Exited) => {}
            Some(_) => bail!("agent {} is still running — land or discard it instead", pane.0),
        }
        let rec = self
            .registry
            .load()
            .into_iter()
            .find(|r| r.agent_id == pane.0)
            .ok_or_else(|| anyhow::anyhow!("no worktree with pane {}", pane.0))?;

        // Capture the live tree before anything destructive happens. `resume_agent` →
        // `finalize_agent` unconditionally overwrites `trees[pane]` with a bare leaf, and
        // `restore_layout` — if handed a tree — rebuilds it by spawning brand-new companion
        // processes. That's correct for `reconcile`'s cold-daemon case (nothing is actually
        // alive to preserve) but wrong here: the daemon is warm, any companions are still
        // running, and handing their shape to `restore_layout` would duplicate them and orphan
        // the originals (still in `panes`/`owner`/`companion_watchers`, no longer referenced by
        // the rebuilt tree, unreapable until the daemon exits). So below we suppress
        // `restore_layout` entirely and reinstate this captured tree ourselves once the agent's
        // own pane is back — reusing the exact same companion ids and `owner` entries, which we
        // never touch.
        let live_tree = self.split_tree_of(pane);

        // Drop the dead pane and its stale exit watcher; `resume_agent` installs fresh ones under
        // the same id. Live companion panes (and their owner/tree bookkeeping) are deliberately
        // left alone — not killed, not replaced.
        if let Some(h) = self.watchers.lock().remove(&pane) {
            h.abort();
        }
        if let Some(h) = self.scanners.lock().remove(&pane) {
            h.abort();
        }
        self.hookless.lock().remove(&pane);
        self.panes.lock().remove(&pane);

        // Suppress `resume_agent`'s `restore_layout` call — we restore the tree ourselves below,
        // from the live snapshot, instead of letting it respawn companions from a persisted one.
        let mut resume_rec = rec.clone();
        resume_rec.tree = None;
        self.resume_agent(&resume_rec)?;

        // Put the real layout back, replacing the bare leaf `finalize_agent` just installed.
        // Fall back to the persisted tree only if the live one is genuinely gone — `split_tree_of`
        // returns `Some(Leaf{pane})` for an ordinary unsplit agent, not `None`, so this only fires
        // if the agent's tree entry vanished from under us entirely, which should not happen while
        // it was `Exited` (only `finish_agent` removes a tree, and nothing else can be tearing this
        // agent down concurrently mid-restart) — but an agent restored with no tree at all would be
        // worse than one restored from a slightly stale snapshot.
        if let Some(tree) = live_tree.or(rec.tree) {
            self.trees.lock().insert(pane, tree);
        }
        self.broadcast_tree(pane);

        Ok(())
    }

    fn register_pane(&self, id: PaneId, pane: Pane) {
        self.panes.lock().insert(id, Arc::new(pane));
    }

    pub fn spawn_pane(&self, cmd: PaneCommand, cols: u16, rows: u16) -> Result<PaneId> {
        let id = self.alloc_id();
        let pane = Pane::spawn(id, cmd, cols, rows, self.backlog_cap)?;
        self.register_pane(id, pane);
        Ok(id)
    }

    /// Provision an isolated worktree, inject the adapter's hooks, and spawn the agent in it.
    pub fn spawn_agent(self: &Arc<Self>, project: &Path, adapter: &dyn AgentAdapter, name: &str) -> Result<PaneId> {
        let _mutation = self.project_mutation.lock();
        // Canonicalize first — the registered-project check compares canonical paths, and on
        // macOS /tmp resolves to /private/tmp.
        let project = project
            .canonicalize()
            .with_context(|| format!("no such project path: {}", project.display()))?;
        if !self.projects.contains(&project) {
            bail!("unknown project: {} — add it first", project.display());
        }
        clowder_workspace::validate_workspace_name(name)?;

        // Fail on a collision with a clear message instead of a raw `git worktree add` error.
        // reconcile prunes a registry record when resume fails but leaves the worktree on disk,
        // so an untracked directory here is a real case, not a hypothetical.
        let wt = project.join(".clowder").join("worktrees").join(name);
        if wt.exists() {
            bail!("a worktree named '{name}' already exists at {} — land/discard it or choose another name", wt.display());
        }
        if branch_exists(&project, &format!("clowder/{name}")) {
            bail!("branch clowder/{name} already exists in {} — choose another name", project.display());
        }

        let task = name;
        let id = self.alloc_id();
        let driver = driver_for(&project);
        let ws = driver.provision(&project, task)?;

        // If any post-provision step fails (e.g. the agent binary isn't on PATH), tear down
        // the freshly-provisioned worktree/branch instead of leaking it — otherwise a retry
        // with the same task name fails at `git worktree add`.
        let pane = match (|| -> Result<Pane> {
            adapter.provision_hooks(&ws.path, id, &self.hook_sock)?;

            let mut cmd = adapter.launch_command(&ws.path);
            cmd.cwd = Some(ws.path.clone());
            cmd.env.push(("CLOWDER_AGENT_ID".into(), id.0.to_string()));
            cmd.env.push(("CLOWDER_HOOK_SOCK".into(), self.hook_sock.to_string_lossy().to_string()));

            Pane::spawn(id, cmd, self.default_cols, self.default_rows, self.backlog_cap)
        })() {
            Ok(p) => p,
            Err(e) => {
                // Nothing was ever landed here; fully clean up (worktree + freshly-created
                // branch) so a retry with the same task name doesn't collide.
                let _ = driver.discard(&ws);
                return Err(e);
            }
        };
        let ws_project = ws.project.clone();
        let ws_path = ws.path.clone();
        let ws_branch = ws.branch.clone();
        let ws_kind = ws.kind;
        self.registry.upsert(crate::registry::AgentRecord {
            agent_id: id.0,
            project: ws_project,
            task: task.to_string(),
            adapter_id: adapter.id().to_string(),
            worktree_path: ws_path,
            branch: ws_branch,
            workspace_kind: ws_kind.as_str().to_string(),
            cols: self.default_cols,
            rows: self.default_rows,
            tree: None,
        });
        self.finalize_agent(id, pane, ws, task, adapter);

        Ok(id)
    }

    /// Register a freshly-spawned (or re-spawned, via `reconcile`) agent pane: pane map,
    /// workspace, in-memory agent metadata, attention, the hookless VT-scanner fallback,
    /// its split tree, and its exit watcher. Does NOT touch the registry — callers own that
    /// (so `reconcile`, which is reading the registry, never re-writes the record it just read).
    fn finalize_agent(
        self: &Arc<Self>,
        id: PaneId,
        pane: Pane,
        ws: Workspace,
        name: &str,
        adapter: &dyn AgentAdapter,
    ) {
        let project = ws.project.to_string_lossy().to_string();
        let branch = ws.branch.clone();
        self.register_pane(id, pane);
        self.workspaces.lock().insert(id, ws);
        self.agents.lock().insert(
            id,
            AgentMeta { project, name: name.to_string(), branch },
        );
        self.set_attention(id, AttentionState::Working);

        if !adapter.provides_hooks() {
            self.hookless.lock().insert(id);
            if let Some(pane_arc) = self.panes.lock().get(&id).cloned() {
                let me = Arc::clone(self);
                let idle = self.content_idle;
                let far = std::time::Duration::from_secs(3600);
                let (snapshot, mut rx) = pane_arc.snapshot_and_subscribe();
                let handle = tokio::spawn(async move {
                    let (mut cols, mut rows) = pane_arc.size();
                    let mut screen = clowder_vt::Screen::new(cols, rows);
                    // Output produced before we subscribed (no lost early signal).
                    if !screen.feed(&snapshot).is_empty()
                        && me.attention_of(id) != Some(AttentionState::NeedsInput)
                    {
                        me.set_attention(id, AttentionState::NeedsInput);
                    }
                    let timer = tokio::time::sleep(idle);
                    tokio::pin!(timer);
                    loop {
                        tokio::select! {
                            r = rx.recv() => match r {
                                Ok(chunk) => {
                                    // BEL/OSC escalate immediately (unchanged behavior).
                                    if !screen.feed(&chunk).is_empty()
                                        && me.attention_of(id) != Some(AttentionState::NeedsInput)
                                    {
                                        me.set_attention(id, AttentionState::NeedsInput);
                                    }
                                    let (nc, nr) = pane_arc.size();
                                    if (nc, nr) != (cols, rows) {
                                        cols = nc; rows = nr;
                                        screen.resize(cols, rows);
                                    }
                                    // New output re-arms the quiescence timer.
                                    timer.as_mut().reset(tokio::time::Instant::now() + idle);
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(_) => break, // pane gone
                            },
                            _ = &mut timer => {
                                // Quiescent: a blocking prompt at rest (not in a full-screen app) → NeedsInput.
                                if !screen.is_alt_screen()
                                    && clowder_vt::is_blocking_prompt(&screen.last_nonempty_line())
                                    && me.attention_of(id) != Some(AttentionState::NeedsInput)
                                {
                                    me.set_attention(id, AttentionState::NeedsInput);
                                }
                                // Park until the next output re-arms it (avoid busy-spin on an elapsed sleep).
                                timer.as_mut().reset(tokio::time::Instant::now() + far);
                            }
                        }
                    }
                });
                self.scanners.lock().insert(id, handle);
            }
        }

        self.trees.lock().insert(id, PaneTree::Leaf { pane: id });
        self.owner.lock().insert(id, id);

        if let Some(pane_arc) = self.panes.lock().get(&id).cloned() {
            let me = Arc::clone(self);
            let handle = tokio::spawn(async move {
                pane_arc.wait_exit().await;
                me.set_attention(id, AttentionState::Exited);
            });
            self.watchers.lock().insert(id, handle);
        }
    }

    pub(crate) fn workspace_of(&self, pane: PaneId) -> Option<Workspace> {
        self.workspaces.lock().get(&pane).cloned()
    }

    /// The working directory for a companion of `root`: an agent's worktree, or a project
    /// terminal's project root.
    fn root_cwd(&self, root: PaneId) -> Option<PathBuf> {
        if let Some(ws) = self.workspaces.lock().get(&root) {
            return Some(ws.path.clone());
        }
        self.term_project.lock().get(&root).cloned()
    }

    /// Kill the agent's process and finalize its workspace (land or discard); drop all
    /// per-pane state.
    fn finish_agent(&self, pane: PaneId, land: bool) -> Result<()> {
        // Cascade: kill every companion pane in this agent's tree.
        let companions: Vec<PaneId> = self
            .trees
            .lock()
            .get(&pane)
            .map(|t| crate::split_tree::leaves(t).into_iter().filter(|p| *p != pane).collect())
            .unwrap_or_default();
        for c in &companions {
            if let Some(p) = self.get(*c) {
                let _ = p.kill();
            }
            self.panes.lock().remove(c);
            self.owner.lock().remove(c);
            if let Some(h) = self.companion_watchers.lock().remove(c) {
                h.abort();
            }
        }
        self.trees.lock().remove(&pane);
        self.owner.lock().remove(&pane);

        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        if let Some(handle) = self.watchers.lock().remove(&pane) {
            handle.abort();
        }
        if let Some(h) = self.scanners.lock().remove(&pane) {
            h.abort();
        }
        self.hookless.lock().remove(&pane);
        if let Some(ws) = self.workspace_of(pane) {
            let driver = driver_for_kind(ws.kind);
            if land {
                driver.land(&ws)?;
            } else {
                driver.discard(&ws)?;
            }
        }
        self.workspaces.lock().remove(&pane);
        self.panes.lock().remove(&pane);
        self.attention.lock().remove(&pane);
        self.agents.lock().remove(&pane);
        self.registry.remove(pane.0);
        let _ = self.removed_tx.send(pane);
        Ok(())
    }

    /// Kill the agent's process and remove its worktree without keeping the branch.
    pub fn teardown_agent(&self, pane: PaneId) -> Result<()> {
        self.finish_agent(pane, false)
    }

    /// Graceful shutdown: abort all background watchers/scanners so killed children can't race
    /// spurious attention/reap events, then kill every child PTY and drop the pane map. Does NOT
    /// finalize (land/discard) any workspace — agents keep their worktrees.
    pub fn shutdown(&self) {
        for (_, h) in self.watchers.lock().drain() {
            h.abort();
        }
        for (_, h) in self.scanners.lock().drain() {
            h.abort();
        }
        for (_, h) in self.companion_watchers.lock().drain() {
            h.abort();
        }
        let panes: Vec<Arc<Pane>> = self.panes.lock().values().cloned().collect();
        for p in panes {
            let _ = p.kill();
        }
        self.panes.lock().clear();
    }

    /// Finalize the agent's work: commit any dirty changes, remove the worktree, keep the branch.
    pub fn land_agent(&self, pane: PaneId) -> Result<()> {
        // finish_agent tolerates a missing workspace (`if let Some(ws)`), so without this guard
        // landing a project terminal would silently succeed and kill it.
        if self.term_project.lock().contains_key(&pane) {
            bail!("pane {} is a project terminal — it has no workspace to land", pane.0);
        }
        self.finish_agent(pane, true)
    }

    /// Throw away the agent's work: remove the worktree and delete its branch.
    pub fn discard_agent(&self, pane: PaneId) -> Result<()> {
        if self.term_project.lock().contains_key(&pane) {
            bail!("pane {} is a project terminal — it has no workspace to discard", pane.0);
        }
        self.finish_agent(pane, false)
    }

    pub fn subscribe_splits(&self) -> broadcast::Receiver<(PaneId, PaneTree)> {
        self.split_tx.subscribe()
    }

    pub fn split_tree_of(&self, agent: PaneId) -> Option<PaneTree> {
        self.trees.lock().get(&agent).cloned()
    }

    /// SplitTreeChanged for `agent`, or an Error event if it has no tree.
    pub fn tree_event(&self, agent: PaneId) -> clowder_proto::ControlEvent {
        match self.split_tree_of(agent) {
            Some(tree) => clowder_proto::ControlEvent::SplitTreeChanged { agent, tree },
            None => clowder_proto::ControlEvent::Error { message: format!("no split tree for {agent:?}") },
        }
    }

    fn broadcast_tree(&self, agent: PaneId) {
        if let Some(tree) = self.split_tree_of(agent) {
            let _ = self.split_tx.send((agent, tree));
        }
    }

    /// Persist the agent's current split tree to its registry record. A bare agent leaf is stored as
    /// `None` (keeps records small); anything with companions is stored literally. Called on every
    /// structural tree change (split/close/reap); ratio drags persist via the coalesced flush instead.
    fn persist_tree(&self, agent: PaneId) {
        let opt = match self.trees.lock().get(&agent) {
            Some(PaneTree::Leaf { pane }) if *pane == agent => None,
            Some(tree) => Some(tree.clone()),
            None => None,
        };
        self.registry.set_tree(agent.0, opt);
    }

    /// Mark an agent's layout dirty (a ratio drag). Coalesced: the periodic flusher persists it.
    fn mark_layout_dirty(&self, agent: PaneId) {
        self.layout_dirty.lock().insert(agent);
    }

    /// Persist every dirty agent's current tree, then clear the dirty set. Skips agents no longer
    /// live (landed/discarded since being marked). Safe to call directly (used by tests + the flusher).
    pub fn flush_dirty_layouts(&self) {
        let dirty: Vec<PaneId> = self.layout_dirty.lock().drain().collect();
        for agent in dirty {
            if self.trees.lock().contains_key(&agent) {
                self.persist_tree(agent);
            }
        }
    }

    /// Spawn the background task that flushes coalesced ratio changes every `LAYOUT_FLUSH_INTERVAL`.
    /// Runs for the daemon's lifetime.
    pub fn spawn_layout_flusher(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(LAYOUT_FLUSH_INTERVAL);
            loop {
                ticker.tick().await;
                me.flush_dirty_layouts();
            }
        })
    }

    fn alloc_split_id(&self) -> SplitId {
        SplitId(self.next_split_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Rebuild an agent's companion layout on reconcile: spawn a fresh shell per companion leaf,
    /// wire owner + reap watchers, install the rebuilt tree, and broadcast. Best-effort — a companion
    /// that fails to spawn collapses only its leaf; a bare agent leaf is a no-op (finalize already set
    /// the single-leaf tree).
    fn restore_layout(self: &Arc<Self>, agent: PaneId, tree: PaneTree, cwd: std::path::PathBuf) {
        if matches!(&tree, PaneTree::Leaf { pane } if *pane == agent) {
            return;
        }
        let shell = self.shell.clone();
        let (cols, rows) = (self.default_cols, self.default_rows);
        let mut spawn_companion = || -> Option<PaneId> {
            self.spawn_pane(companion_command(shell.clone(), cwd.clone()), cols, rows).ok()
        };
        let mut alloc_split = || self.alloc_split_id();
        let (rebuilt, companions) =
            crate::split_tree::rebuild_for_restore(&tree, agent, &mut spawn_companion, &mut alloc_split);

        // Install owner + tree + broadcast BEFORE spawning any reap watcher (mirrors `split_pane`):
        // `wait_exit()` returns immediately if the child already exited, so no exit is missed even
        // when the watcher registers late — but registering it early risks `reap_companion` firing
        // against the still-bare pre-restore tree and dropping a companion the rebuilt tree still
        // references, leaving a phantom leaf.
        for c in &companions {
            self.owner.lock().insert(*c, agent);
        }
        self.trees.lock().insert(agent, rebuilt);
        self.broadcast_tree(agent);

        for c in companions {
            if let Some(pane_arc) = self.get(c) {
                let me = Arc::clone(self);
                let handle = tokio::spawn(async move {
                    pane_arc.wait_exit().await;
                    me.reap_companion(c);
                });
                self.companion_watchers.lock().insert(c, handle);
            }
        }
    }

    /// Split `target` (a leaf) by spawning a companion shell in its agent's worktree.
    pub fn split_pane(self: &Arc<Self>, target: PaneId, direction: SplitDirection) -> Result<PaneId> {
        let agent = *self
            .owner
            .lock()
            .get(&target)
            .ok_or_else(|| anyhow::anyhow!("unknown pane {target:?}"))?;
        let path = self
            .root_cwd(agent)
            .ok_or_else(|| anyhow::anyhow!("no workspace for agent {agent:?}"))?;
        let companion = self.spawn_pane(
            companion_command(self.shell.clone(), path),
            self.default_cols,
            self.default_rows,
        )?;
        let sid = self.alloc_split_id();
        {
            let mut trees = self.trees.lock();
            let tree = trees
                .get_mut(&agent)
                .ok_or_else(|| anyhow::anyhow!("no split tree for {agent:?}"))?;
            // A concurrent companion-crash reap may have already removed `target` from the tree
            // (e.g. splitting a companion the instant its process dies); that's a benign race, not
            // a bug — the freshly-spawned companion self-heals via its own reap when it exits.
            let _ = crate::split_tree::split_leaf(tree, target, companion, direction, sid);
        }
        self.owner.lock().insert(companion, agent);
        self.broadcast_tree(agent);
        self.persist_tree(agent);

        // Reap the companion if its process exits/crashes (mirrors the per-agent watcher). Registered
        // after owner+tree are set so `reap_companion` always finds the owner. `wait_exit()` returns
        // immediately if the child already exited, so no exit is missed even if we register late.
        if let Some(pane_arc) = self.get(companion) {
            let me = Arc::clone(self);
            let handle = tokio::spawn(async move {
                pane_arc.wait_exit().await;
                me.reap_companion(companion);
            });
            self.companion_watchers.lock().insert(companion, handle);
        }

        Ok(companion)
    }

    /// Remove a companion pane whose process exited/crashed: collapse its leaf out of the owning
    /// agent's tree and broadcast the change. Idempotent — a no-op if the pane is already gone
    /// (explicit `close_pane`/`teardown` won the race) or if `pane` is an agent's own leaf.
    pub(crate) fn reap_companion(&self, pane: PaneId) {
        let agent = match self.owner.lock().get(&pane).copied() {
            Some(a) => a,
            None => return, // already removed
        };
        if agent == pane {
            return; // an agent's own leaf is never reaped as a companion
        }
        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        self.panes.lock().remove(&pane);
        if let Some(tree) = self.trees.lock().get_mut(&agent) {
            let _ = crate::split_tree::remove_leaf(tree, pane);
        }
        self.owner.lock().remove(&pane);
        self.companion_watchers.lock().remove(&pane);
        self.broadcast_tree(agent);
        self.persist_tree(agent);
    }

    /// Close a companion pane (collapsing the tree), or teardown the agent if `pane` is one.
    /// Returns Some(agent) if a companion was closed, None if an agent was torn down.
    pub fn close_pane(&self, pane: PaneId) -> Result<Option<PaneId>> {
        if self.term_project.lock().contains_key(&pane) {
            // A project terminal's root: kill it and forget it, rather than taking the agent
            // teardown path (which would emit AgentRemoved for something that is not a worktree).
            if let Some(p) = self.get(pane) { let _ = p.kill(); }
            self.forget_project_terminal(pane);
            return Ok(None);
        }
        let is_agent = self.trees.lock().contains_key(&pane);
        if is_agent {
            self.teardown_agent(pane)?;
            return Ok(None);
        }
        let agent = *self
            .owner
            .lock()
            .get(&pane)
            .ok_or_else(|| anyhow::anyhow!("unknown pane {pane:?}"))?;
        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        self.panes.lock().remove(&pane);
        if let Some(h) = self.companion_watchers.lock().remove(&pane) {
            h.abort();
        }
        if let Some(tree) = self.trees.lock().get_mut(&agent) {
            // A concurrent companion-crash reap may have already collapsed this leaf out of the
            // tree; that's a benign race (whichever of close/reap runs first wins), not a bug.
            let _ = crate::split_tree::remove_leaf(tree, pane);
        }
        self.owner.lock().remove(&pane);
        self.broadcast_tree(agent);
        self.persist_tree(agent);
        Ok(Some(agent))
    }

    /// Move a divider. Returns the owning agent so callers can emit its tree.
    pub fn set_split_ratio(&self, split: SplitId, ratio: f32) -> Result<PaneId> {
        let mut found = None;
        {
            let mut trees = self.trees.lock();
            for (agent, tree) in trees.iter_mut() {
                if crate::split_tree::set_ratio(tree, split, ratio) {
                    found = Some(*agent);
                    break;
                }
            }
        }
        let agent = found.ok_or_else(|| anyhow::anyhow!("unknown split {split:?}"))?;
        self.broadcast_tree(agent);
        self.mark_layout_dirty(agent);
        Ok(agent)
    }

    /// The agent owning any leaf (or the agent itself).
    pub fn owner_of(&self, pane: PaneId) -> Option<PaneId> {
        self.owner.lock().get(&pane).copied()
    }

    pub fn list_worktrees(&self) -> Vec<clowder_proto::WorktreeInfo> {
        let agents = self.agents.lock();
        let attention = self.attention.lock();
        let mut out: Vec<clowder_proto::WorktreeInfo> = agents
            .iter()
            .map(|(pane, meta)| clowder_proto::WorktreeInfo {
                pane: *pane,
                project: meta.project.clone(),
                name: meta.name.clone(),
                branch: meta.branch.clone(),
                state: attention.get(pane).copied().unwrap_or(clowder_proto::AttentionState::Working),
            })
            .collect();
        out.sort_by(|a, b| (a.project.as_str(), a.pane.0).cmp(&(b.project.as_str(), b.pane.0)));
        out
    }

    /// The adapters a client may spawn (registry descriptor ids + labels).
    pub fn list_adapters(&self) -> Vec<clowder_proto::AdapterInfo> {
        crate::adapter_descriptors()
            .iter()
            .map(|d| clowder_proto::AdapterInfo { id: d.id.to_string(), display_name: d.display_name.to_string() })
            .collect()
    }

    fn get(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.lock().get(&id).cloned()
    }

    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                if let Some(line) = crate::logging::conn_error_line("client", me.handle_conn(stream).await) {
                    tracing::warn!("{line}");
                }
            });
        }
    }

    pub async fn handle_conn<S>(self: Arc<Self>, stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let mut msgs = MsgStream::new(stream);
        // First message must be Attach.
        let pane = loop {
            match msgs.recv::<ClientToDaemon>().await? {
                Some(ClientToDaemon::Attach { pane }) => match self.get(pane) {
                    Some(p) => break p,
                    None => return Ok(()), // unknown pane: end session
                },
                Some(ClientToDaemon::ListWorktrees) => {
                    return self.handle_control(msgs).await;
                }
                Some(_) => continue, // ignore until attached
                None => return Ok(()),
            }
        };

        let (cols, rows) = pane.size();
        // Subscribe to attention BEFORE sending Attached/backlog: a state change triggered right
        // after the client observes the attach must be buffered by the subscription, not dropped
        // (the old subscribe-after-backlog order lost it under load).
        let mut att_rx = self.subscribe_attention();
        msgs.send(&DaemonToClient::Attached { pane: pane.id(), cols, rows }).await?;
        // Deliver the current attention state so a client attaching to an already-needy agent
        // learns it immediately (future changes still arrive via `att_rx` in the loop below).
        if let Some(state) = self.attention_of(pane.id()) {
            msgs.send(&DaemonToClient::AttentionChanged { pane: pane.id(), state }).await?;
        }

        let (snap, mut sub) = pane.snapshot_and_subscribe();
        msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes: snap }).await?;

        loop {
            tokio::select! {
                live = sub.recv() => {
                    match live {
                        Ok(bytes) => msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                incoming = msgs.recv::<ClientToDaemon>() => {
                    match incoming? {
                        Some(ClientToDaemon::Input { bytes, .. }) => {
                            let _ = pane.write_input(&bytes);
                            let pid = pane.id();
                            // User engaged with an agent whose attention was "waiting" → back to Working.
                            // Applies to all agents: hook-less (VT/BEL) AND hook'd tools like Codex that
                            // only emit a turn-complete signal and no "resumed" event.
                            if matches!(
                                self.attention_of(pid),
                                Some(AttentionState::NeedsInput | AttentionState::Completed)
                            ) {
                                self.set_attention(pid, AttentionState::Working);
                            }
                        }
                        Some(ClientToDaemon::Resize { cols, rows, .. }) => { let _ = pane.resize(cols, rows); }
                        Some(ClientToDaemon::Detach) | None => break,
                        Some(ClientToDaemon::Attach { .. }) => continue,
                        Some(ClientToDaemon::ListWorktrees) => continue,
                    }
                }
                att = att_rx.recv() => {
                    match att {
                        Ok((p, state)) if p == pane.id() => {
                            msgs.send(&DaemonToClient::AttentionChanged { pane: p, state }).await?;
                        }
                        Ok(_) => continue,                        // another pane's attention
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => {}                              // attention channel closed; ignore
                    }
                }
                code = pane.wait_exit() => {
                    // Drain output already buffered before ending the session, so an agent's
                    // final lines aren't dropped when it exits right after printing.
                    while let Ok(bytes) = sub.try_recv() {
                        msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes }).await?;
                    }
                    msgs.send(&DaemonToClient::PaneExited { pane: pane.id(), code }).await?;
                    break;
                }
            }
        }
        Ok(())
    }

    async fn handle_control<S>(self: Arc<Self>, mut msgs: MsgStream<S>) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        // Snapshot the worktree list, then stream every attention change.
        let mut att_rx = self.subscribe_attention();
        let mut removed_rx = self.subscribe_removed();
        msgs.send(&DaemonToClient::WorktreeList { worktrees: self.list_worktrees() }).await?;
        loop {
            tokio::select! {
                att = att_rx.recv() => {
                    match att {
                        Ok((pane, state)) => {
                            msgs.send(&DaemonToClient::AttentionChanged { pane, state }).await?;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break, // attention channel closed
                    }
                }
                removed = removed_rx.recv() => {
                    match removed {
                        Ok(pane) => { msgs.send(&DaemonToClient::AgentRemoved { pane }).await?; }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                incoming = msgs.recv::<ClientToDaemon>() => {
                    match incoming? {
                        Some(ClientToDaemon::ListWorktrees) => {
                            // Client asked to refresh the list.
                            msgs.send(&DaemonToClient::WorktreeList { worktrees: self.list_worktrees() }).await?;
                        }
                        Some(_) => continue,     // control conn ignores pane ops
                        None => break,           // client disconnected
                    }
                }
            }
        }
        Ok(())
    }
}

/// Does `branch` already exist in `project`? Best-effort: a false negative just means the
/// underlying driver reports the collision instead, which is the pre-M10b behaviour.
fn branch_exists(project: &Path, branch: &str) -> bool {
    use clowder_workspace::WorkspaceKind;
    match clowder_workspace::detect_kind(project) {
        Some(WorkspaceKind::Jj) => std::process::Command::new("jj")
            .arg("-R").arg(project).args(["bookmark", "list", "-r", branch])
            .output().map(|o| o.status.success() && !o.stdout.is_empty()).unwrap_or(false),
        _ => std::process::Command::new("git")
            .arg("-C").arg(project).args(["branch", "--list", branch])
            .output().map(|o| !o.stdout.is_empty()).unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_repo;
    use std::time::Duration;

    fn sh(script: &str) -> PaneCommand {
        PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: vec![],
        }
    }

    #[tokio::test]
    async fn small_backlog_cap_bounds_the_buffer() {
        // A daemon with a tiny configured backlog cap must bound the pane's byte-tail even
        // though the child emits far more than the cap.
        let mut d = Daemon::new_with(Arc::new(crate::FakeNotifier::new()), "/tmp/unused-cap.sock".into());
        d.backlog_cap = 4096; // pub(crate) — set before wrapping in Arc
        let daemon = Arc::new(d);
        let pane = daemon
            .spawn_pane(sh("yes ABCDEFGHIJKLMNOPQRST | head -c 20000"), 80, 24)
            .unwrap();
        let backlog_len = || daemon.panes.lock().get(&pane).unwrap().backlog().len();
        // Wait until the buffer fills to the cap (the child prints ~20000 bytes >> 4096).
        for _ in 0..100 {
            if backlog_len() >= 4096 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let len = backlog_len();
        assert!(len <= 4096, "backlog {len} exceeded the configured cap of 4096");
        assert!(len >= 2048, "backlog {len} never filled — drain path not exercised");
    }

    #[tokio::test]
    async fn client_attaches_and_receives_output() {
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("cat"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();

        // Expect Attached, then a (possibly empty) backlog Output.
        let attached: DaemonToClient = client.recv().await.unwrap().unwrap();
        assert!(matches!(attached, DaemonToClient::Attached { .. }));
        let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

        client
            .send(&ClientToDaemon::Input { pane, bytes: b"echo hi\n".to_vec() })
            .await
            .unwrap();

        let mut seen = Vec::new();
        for _ in 0..50 {
            if let Ok(Ok(Some(DaemonToClient::Output { bytes, .. }))) =
                tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
            {
                seen.extend_from_slice(&bytes);
                if seen.windows(2).any(|w| w == b"hi") {
                    break;
                }
            }
        }
        assert!(seen.windows(2).any(|w| w == b"hi"), "did not receive echoed output");
    }

    #[tokio::test]
    async fn pane_survives_detach_and_replays_on_reattach() {
        use std::time::Duration;

        let daemon = Arc::new(Daemon::new());
        // A shell that appends a line every 100ms to prove it keeps running while detached.
        let pane = daemon
            .spawn_pane(sh("i=0; while true; do i=$((i+1)); echo line$i; sleep 0.1; done"), 80, 24)
            .unwrap();

        // First client attaches, collects some output, then detaches.
        {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            let d = daemon.clone();
            let h = tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

            let mut client = MsgStream::<_>::new(client_io);
            client.send(&ClientToDaemon::Attach { pane }).await.unwrap();
            let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
            let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

            // Read a little live output.
            let mut seen = Vec::new();
            for _ in 0..30 {
                if let Ok(Ok(Some(DaemonToClient::Output { bytes, .. }))) =
                    tokio::time::timeout(Duration::from_millis(100), client.recv::<DaemonToClient>()).await
                {
                    seen.extend_from_slice(&bytes);
                    if seen.windows(5).any(|w| w == b"line1") {
                        break;
                    }
                }
            }
            assert!(seen.windows(5).any(|w| w == b"line1"), "first attach saw no output");

            client.send(&ClientToDaemon::Detach).await.unwrap();
            let _ = h.await; // session ends; pane must keep running
        }

        // Wait — by condition, not a fixed sleep — until the detached shell has produced a line that
        // can only exist well after the first client detached. A fixed 400ms wait flaked on slow /
        // loaded CI runners, where the shell (one line per 100ms) hadn't reached line4 yet.
        let mut produced_while_detached = false;
        for _ in 0..200 {
            let has_line4 = daemon
                .panes
                .lock()
                .get(&pane)
                .map(|p| p.backlog().windows(5).any(|w| w == b"line4"))
                .unwrap_or(false);
            if has_line4 {
                produced_while_detached = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(produced_while_detached, "shell did not keep producing output while detached");

        // Second client reattaches; the backlog replay must contain later lines
        // that were produced WHILE no client was attached.
        {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            let d = daemon.clone();
            tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

            let mut client = MsgStream::<_>::new(client_io);
            client.send(&ClientToDaemon::Attach { pane }).await.unwrap();
            let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
            let backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

            let bytes = match backlog {
                DaemonToClient::Output { bytes, .. } => bytes,
                other => panic!("expected backlog Output, got {other:?}"),
            };
            // At least line4+ should exist, proving the pane produced output while detached.
            assert!(
                bytes.windows(5).any(|w| w == b"line4"),
                "reattach backlog did not include output produced while detached: {:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }

    #[tokio::test]
    async fn attached_client_gets_attention_changed() {
        use clowder_proto::AttentionState;
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("sleep 30"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();
        let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
        let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

        // Flip attention; the attached client must receive AttentionChanged.
        daemon.set_attention(pane, AttentionState::NeedsInput);

        let mut got = None;
        for _ in 0..50 {
            if let Ok(Ok(Some(msg))) =
                tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
            {
                if let DaemonToClient::AttentionChanged { state, .. } = msg {
                    got = Some(state);
                    break;
                }
            }
        }
        assert_eq!(got, Some(AttentionState::NeedsInput));
    }

    #[tokio::test]
    async fn attach_to_already_needy_pane_delivers_current_attention() {
        use clowder_proto::AttentionState;
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("sleep 30"), 80, 24).unwrap();
        // Attention is set BEFORE the client attaches — the client must still learn it.
        daemon.set_attention(pane, AttentionState::NeedsInput);

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();

        // Within the first few frames after Attach, an AttentionChanged{NeedsInput} must arrive.
        let mut got = None;
        for _ in 0..50 {
            match tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await {
                Ok(Ok(Some(DaemonToClient::AttentionChanged { state, .. }))) => { got = Some(state); break; }
                Ok(Ok(Some(_))) => {}                 // Attached / Output
                Ok(Ok(None)) | Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        assert_eq!(got, Some(AttentionState::NeedsInput), "attaching client must learn current attention");
    }

    #[tokio::test]
    async fn client_gets_pane_exited_when_child_exits() {
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("exit 3"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();

        // Expect a PaneExited to arrive (rather than the session hanging forever).
        let mut exited = false;
        for _ in 0..100 {
            match tokio::time::timeout(Duration::from_millis(100), client.recv::<DaemonToClient>()).await {
                Ok(Ok(Some(DaemonToClient::PaneExited { .. }))) => { exited = true; break; }
                Ok(Ok(Some(_))) => {}                 // Attached / Output / AttentionChanged
                Ok(Ok(None)) | Ok(Err(_)) => break,   // stream closed / recv error
                Err(_) => continue,                    // 100ms window elapsed; keep polling
            }
        }
        assert!(exited, "client never received PaneExited on child exit");
    }

    #[tokio::test]
    async fn client_gets_final_output_then_pane_exited() {
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("printf BYE; sleep 0.3; exit 0"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();

        let mut saw_bye = false;
        let mut saw_exit = false;
        for _ in 0..100 {
            match tokio::time::timeout(Duration::from_millis(100), client.recv::<DaemonToClient>()).await {
                Ok(Ok(Some(DaemonToClient::Output { bytes, .. }))) => {
                    if bytes.windows(3).any(|w| w == b"BYE") { saw_bye = true; }
                }
                Ok(Ok(Some(DaemonToClient::PaneExited { .. }))) => { saw_exit = true; break; }
                Ok(Ok(Some(_))) => {}   // Attached / AttentionChanged
                Ok(Ok(None)) | Ok(Err(_)) => break, // stream closed / recv error
                Err(_) => continue,     // this iteration's 100ms window elapsed; keep polling
            }
        }
        assert!(saw_bye, "did not receive final output before exit");
        assert!(saw_exit, "did not receive PaneExited");
    }

    #[tokio::test]
    async fn list_worktrees_reports_project_name_branch_and_state() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use clowder_proto::AttentionState;
        use std::sync::Arc as StdArc;

        let repo = init_repo();
        let state = tempfile::tempdir().unwrap();
        let daemon = StdArc::new(Daemon::new_with_paths(
            StdArc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-listagents.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let pane = daemon.spawn_agent(repo.path(), &adapter, "task-a").unwrap();
        daemon.set_attention(pane, AttentionState::NeedsInput);

        let list = daemon.list_worktrees();
        assert_eq!(list.len(), 1);
        let a = &list[0];
        assert_eq!(a.pane, pane);
        assert_eq!(a.name, "task-a");
        assert_eq!(a.branch, "clowder/task-a");
        // project is now the FULL, CANONICAL path (spawn_agent canonicalizes it) — two repos
        // with the same dir name must not collapse into one sidebar group.
        assert_eq!(a.project, repo.path().canonicalize().unwrap().to_string_lossy());
        assert_eq!(a.state, AttentionState::NeedsInput);

        daemon.teardown_agent(pane).unwrap();
        assert!(daemon.list_worktrees().is_empty());
    }

    #[tokio::test]
    async fn spawn_writes_registry_and_finish_removes_it() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let _state_lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));

        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-m9.sock"),
            statedir.path().join("agents.json"),
            statedir.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();

        let recs = crate::registry::Registry::new(statedir.path().join("agents.json")).load();
        assert_eq!(recs.iter().filter(|r| r.agent_id == id.0).count(), 1);
        assert_eq!(recs[0].adapter_id, "synthetic");

        daemon.discard_agent(id).unwrap();
        assert!(crate::registry::Registry::new(statedir.path().join("agents.json")).load().is_empty());

        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    #[ignore = "load-sensitive: spawns real /bin/sh + git worktrees; run via `cargo test -- --ignored --test-threads=1`"]
    async fn reconcile_respawns_recorded_agents_and_prunes_missing() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let _state_lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));
        let state_path = statedir.path().join("agents.json");
        let projects_path = statedir.path().join("projects.json");

        // First daemon: spawn an agent so a worktree + registry record exist.
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-reconcile1.sock"),
            state_path.clone(),
            projects_path.clone(),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();
        let worktree_path = daemon.workspace_of(id).unwrap().path;

        // Simulate a fresh daemon (e.g. after a restart): a NEW Daemon on the same state file,
        // with no in-memory agents, reconciling from the registry alone. The project store
        // (like the registry) persists across a restart, so it points at the same file too.
        let d2 = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-reconcile2.sock"),
            state_path.clone(),
            projects_path.clone(),
        ));
        d2.reconcile();
        let list = d2.list_worktrees();
        assert_eq!(list.len(), 1, "reconcile must re-register the recorded agent");
        assert_eq!(list[0].pane, id, "re-registered under the original id");
        assert_eq!(list[0].name, "demo");

        // New spawns on d2 must not collide with the restored id.
        let fresh = d2.spawn_agent(repo.path(), &adapter, "fresh").unwrap();
        assert_ne!(fresh, id, "next_id must be bumped above restored ids");
        d2.shutdown();

        // Now corrupt: remove the worktree dir out from under the registry, then reconcile a
        // third daemon → the stale record is pruned, both in memory and on disk.
        std::fs::remove_dir_all(&worktree_path).unwrap();
        let d3 = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-reconcile3.sock"),
            state_path.clone(),
            projects_path.clone(),
        ));
        d3.reconcile();
        assert!(
            d3.list_worktrees().iter().all(|a| a.pane != id),
            "agent whose worktree is gone must be pruned"
        );
        assert!(
            crate::registry::Registry::new(state_path).load().iter().all(|r| r.agent_id != id.0),
            "pruned record must not survive on disk"
        );
        d3.shutdown();

        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    async fn reconcile_restores_split_layout() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;
        use clowder_proto::{PaneTree, SplitDirection};

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let _state_lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));
        let state_path = statedir.path().join("agents.json");
        let projects_path = statedir.path().join("projects.json");

        let d1 = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-restore1.sock"),
            state_path.clone(),
            projects_path.clone(),
        ));
        d1.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
                cwd: None, env: vec![],
            },
        };
        let id = d1.spawn_agent(repo.path(), &adapter, "demo").unwrap();
        d1.split_pane(id, SplitDirection::Right).unwrap();
        // set + flush a non-default ratio so we can assert it round-trips.
        let sid = match d1.split_tree_of(id).unwrap() { PaneTree::Split { id, .. } => id, _ => panic!() };
        d1.set_split_ratio(sid, 0.3).unwrap();
        d1.flush_dirty_layouts();

        // Fresh daemon over the same state file → reconcile rebuilds the layout.
        let d2 = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-restore2.sock"),
            state_path,
            projects_path,
        ));
        d2.reconcile();

        let tree = d2.split_tree_of(id).expect("agent tree restored");
        let ls = crate::split_tree::leaves(&tree);
        assert_eq!(ls.len(), 2, "two leaves restored");
        assert!(ls.contains(&id), "agent leaf id preserved");
        match tree {
            PaneTree::Split { ratio, first, .. } => {
                assert!((ratio - 0.3).abs() < 1e-6, "ratio restored: {ratio}");
                assert_eq!(*first, PaneTree::Leaf { pane: id }, "agent is the first leaf");
            }
            _ => panic!("expected split"),
        }
        d2.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    async fn split_and_close_persist_the_tree() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;
        use clowder_proto::SplitDirection;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let state_path = statedir.path().join("agents.json");
        let _state_lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", &state_path);

        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-persist.sock"),
            state_path.clone(),
            statedir.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
                cwd: None, env: vec![],
            },
        };
        let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();

        // After a split, the record's tree is a 2-leaf split.
        let companion = daemon.split_pane(id, SplitDirection::Right).unwrap();
        let recs = crate::registry::Registry::new(state_path.clone()).load();
        let tree = recs.iter().find(|r| r.agent_id == id.0).unwrap().tree.clone();
        assert!(matches!(tree, Some(clowder_proto::PaneTree::Split { .. })), "split persisted: {tree:?}");
        assert_eq!(crate::split_tree::leaves(tree.as_ref().unwrap()).len(), 2);

        // After closing the companion, the tree collapses back and is persisted as None (bare leaf).
        daemon.close_pane(companion).unwrap();
        let recs = crate::registry::Registry::new(state_path.clone()).load();
        assert_eq!(recs.iter().find(|r| r.agent_id == id.0).unwrap().tree, None);

        daemon.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    async fn control_conn_lists_agents_and_streams_attention() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use clowder_proto::AttentionState;
        use std::process::Command as PCommand;
        use std::time::Duration;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-control.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let pane = daemon.spawn_agent(repo.path(), &adapter, "task-a").unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_conn(server_io).await; });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::ListWorktrees).await.unwrap();

        // First reply is the worktree list.
        match client.recv::<DaemonToClient>().await.unwrap().unwrap() {
            DaemonToClient::WorktreeList { worktrees } => {
                assert_eq!(worktrees.len(), 1);
                assert_eq!(worktrees[0].pane, pane);
                assert_eq!(worktrees[0].name, "task-a");
            }
            other => panic!("expected WorktreeList, got {other:?}"),
        }

        // A later attention change streams over the SAME control connection,
        // even though this client is not "attached" to the pane.
        daemon.set_attention(pane, AttentionState::NeedsInput);
        let mut saw = None;
        for _ in 0..40 {
            if let Ok(Ok(Some(DaemonToClient::AttentionChanged { pane: p, state }))) =
                tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
            {
                if p == pane { saw = Some(state); break; }
            }
        }
        assert_eq!(saw, Some(AttentionState::NeedsInput));

        daemon.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn agent_marked_exited_on_process_exit() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use clowder_proto::AttentionState;
        use std::process::Command as PCommand;
        use std::time::Duration;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-reaper.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "exit 0".into()],
                cwd: None,
                env: vec![],
            },
        };
        let pane = daemon.spawn_agent(repo.path(), &adapter, "task-x").unwrap();

        // No client attached: the daemon-side watcher must still flip attention to Exited.
        let mut exited = false;
        for _ in 0..100 {
            if daemon.attention_of(pane) == Some(AttentionState::Exited) { exited = true; break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(exited, "agent was not marked Exited after its process exited");
        // It stays in the list (mark-exited-and-keep), still reported with Exited state.
        let list = daemon.list_worktrees();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].state, AttentionState::Exited);

        daemon.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn teardown_of_running_agent_does_not_leave_spurious_exited() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;
        use std::time::Duration;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-teardown-race.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let pane = daemon.spawn_agent(repo.path(), &adapter, "task-r").unwrap();

        // Tear down while still running (kills the child).
        daemon.teardown_agent(pane).unwrap();

        // Give the killed child time to die + any un-aborted watcher time to fire.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // The watcher was aborted, so no Exited entry was re-inserted after teardown.
        assert_eq!(daemon.attention_of(pane), None, "watcher left a spurious attention entry after teardown");
    }

    #[tokio::test]
    async fn control_conn_gets_agent_removed_on_teardown() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;
        use std::time::Duration;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-removed.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let pane = daemon.spawn_agent(repo.path(), &adapter, "task-a").unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_conn(server_io).await; });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::ListWorktrees).await.unwrap();
        // Drain the initial WorktreeList.
        let _ = client.recv::<DaemonToClient>().await.unwrap().unwrap();

        daemon.teardown_agent(pane).unwrap();

        let mut removed = None;
        for _ in 0..40 {
            if let Ok(Ok(Some(DaemonToClient::AgentRemoved { pane: p }))) =
                tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
            {
                removed = Some(p);
                break;
            }
        }
        assert_eq!(removed, Some(pane));
    }

    /// Temp git repo + a daemon wired up the same way the other integration tests build one,
    /// with the repo already registered as a project. Returns the state TempDir too — it must
    /// outlive the daemon, since the project/registry stores re-read their file on every call.
    fn daemon_with_repo() -> (Arc<Daemon>, tempfile::TempDir, tempfile::TempDir) {
        use crate::FakeNotifier;

        let repo = init_repo();
        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-split-tree.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        (daemon, repo, state)
    }

    #[test]
    fn companion_command_uses_shell_and_worktree_cwd() {
        let cmd = companion_command("/bin/zsh".into(), std::path::PathBuf::from("/tmp/wt"));
        assert_eq!(cmd.program, "/bin/zsh");
        assert_eq!(cmd.cwd, Some(std::path::PathBuf::from("/tmp/wt")));
        assert!(cmd.args.is_empty());
        assert!(cmd.env.is_empty()); // no hook env on a companion
    }

    #[tokio::test]
    async fn split_close_and_teardown_manage_the_tree() {
        use crate::split_tree;

        // temp git repo + daemon with the shell adapter (reuse the existing helpers/pattern)
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon
            .spawn_agent(
                repo.path(),
                &crate::agent::SyntheticAdapter {
                    command: PaneCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "sleep 30".into()],
                        cwd: None,
                        env: vec![],
                    },
                },
                "task",
            )
            .unwrap();

        // fresh tree is a lone leaf
        assert_eq!(daemon.split_tree_of(agent), Some(PaneTree::Leaf { pane: agent }));

        // split → companion pane exists, tree is a split with two leaves
        let mut rx = daemon.subscribe_splits();
        let comp = daemon.split_pane(agent, SplitDirection::Right).unwrap();
        assert!(daemon.get(comp).is_some(), "companion pane must exist");
        let tree = daemon.split_tree_of(agent).unwrap();
        assert_eq!(split_tree::leaves(&tree), vec![agent, comp]);
        let (bagent, _btree) = rx.try_recv().expect("SplitTreeChanged broadcast");
        assert_eq!(bagent, agent);

        // nested split on the companion → 3 leaves
        let comp2 = daemon.split_pane(comp, SplitDirection::Down).unwrap();
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()).len(), 3);

        // close one companion → collapses, pane gone
        daemon.close_pane(comp2).unwrap();
        assert!(daemon.get(comp2).is_none());
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()), vec![agent, comp]);

        // teardown the agent → all companions gone, tree dropped
        daemon.teardown_agent(agent).unwrap();
        assert!(daemon.get(comp).is_none(), "companion must be killed on teardown");
        assert!(daemon.split_tree_of(agent).is_none());
    }

    #[tokio::test]
    async fn teardown_kills_multiple_live_companions() {
        use crate::split_tree;

        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon
            .spawn_agent(
                repo.path(),
                &crate::agent::SyntheticAdapter {
                    command: PaneCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "sleep 30".into()],
                        cwd: None,
                        env: vec![],
                    },
                },
                "task",
            )
            .unwrap();

        // Two companions, BOTH live at teardown (neither closed first).
        let c1 = daemon.split_pane(agent, SplitDirection::Right).unwrap();
        let c2 = daemon.split_pane(agent, SplitDirection::Down).unwrap();
        assert!(daemon.get(c1).is_some());
        assert!(daemon.get(c2).is_some());
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()).len(), 3);

        daemon.teardown_agent(agent).unwrap();

        assert!(daemon.get(c1).is_none(), "companion 1 must be killed on teardown");
        assert!(daemon.get(c2).is_none(), "companion 2 must be killed on teardown");
        assert!(daemon.split_tree_of(agent).is_none());
    }

    // A test adapter that claims hooks but launches a benign command (so we can assert the
    // scanner is NOT spawned for hook'd agents without needing the `claude` binary).
    struct HookedTestAdapter { cmd: PaneCommand }
    impl crate::agent::AgentAdapter for HookedTestAdapter {
        fn id(&self) -> &'static str { "hooked-test" }
        fn provides_hooks(&self) -> bool { true }
        fn provision_hooks(&self, _w: &std::path::Path, _a: PaneId, _s: &std::path::Path) -> anyhow::Result<()> { Ok(()) }
        fn launch_command(&self, _w: &std::path::Path) -> PaneCommand { self.cmd.clone() }
    }

    fn bell_then_sleep() -> PaneCommand {
        PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "printf '\\a'; sleep 30".into()], cwd: None, env: vec![] }
    }

    #[tokio::test]
    async fn hookless_agent_bell_sets_needs_input() {
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter { command: bell_then_sleep() }, "t").unwrap();
        let mut ok = false;
        for _ in 0..100 {
            if daemon.attention_of(agent) == Some(AttentionState::NeedsInput) { ok = true; break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(ok, "a BEL from a hook-less agent should set NeedsInput");
    }

    #[tokio::test]
    async fn hooked_agent_bell_is_ignored() {
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &HookedTestAdapter { cmd: bell_then_sleep() }, "t").unwrap();
        // give the BEL time to be produced; attention must stay Working (no scanner).
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(daemon.attention_of(agent), Some(AttentionState::Working));
    }

    #[tokio::test]
    async fn input_clears_hookless_needs_input_to_working() {
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter { command: bell_then_sleep() }, "t").unwrap();
        // wait for NeedsInput
        for _ in 0..100 {
            if daemon.attention_of(agent) == Some(AttentionState::NeedsInput) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(daemon.attention_of(agent), Some(AttentionState::NeedsInput));

        // Attach a client and send Input (drives handle_conn's input arm), like
        // client_attaches_and_receives_output; then assert it clears to Working.
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_conn(server_io).await; });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane: agent }).await.unwrap();
        let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
        let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

        client.send(&ClientToDaemon::Input { pane: agent, bytes: b"x".to_vec() }).await.unwrap();

        for _ in 0..100 {
            if daemon.attention_of(agent) == Some(AttentionState::Working) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(daemon.attention_of(agent), Some(AttentionState::Working), "input should clear NeedsInput");
    }

    #[tokio::test]
    async fn input_clears_hooked_completed_to_working() {
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon
            .spawn_agent(
                repo.path(),
                &HookedTestAdapter {
                    cmd: PaneCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "sleep 30".into()],
                        cwd: None,
                        env: vec![],
                    },
                },
                "task-a",
            )
            .unwrap();
        // A hook'd agent is NOT in `hookless`.
        daemon.set_attention(agent, AttentionState::Completed);
        assert_eq!(daemon.attention_of(agent), Some(AttentionState::Completed));

        // Attach a client and send Input (drives handle_conn's input arm), like
        // input_clears_hookless_needs_input_to_working does.
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_conn(server_io).await; });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane: agent }).await.unwrap();
        let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
        let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

        client.send(&ClientToDaemon::Input { pane: agent, bytes: b"x".to_vec() }).await.unwrap();

        let mut ok = false;
        for _ in 0..50 {
            if daemon.attention_of(agent) == Some(AttentionState::Working) { ok = true; break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(ok, "input to a hook'd Completed agent must clear to Working");
    }

    fn branch_exists(repo: &std::path::Path, name: &str) -> bool {
        let out = std::process::Command::new("git").arg("-C").arg(repo).args(["branch", "--list", name]).output().unwrap();
        !out.stdout.is_empty()
    }

    #[tokio::test]
    async fn land_agent_keeps_branch_and_removes_agent() {
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "task-a").unwrap();
        // write some work into the worktree
        let ws = daemon.workspace_of(agent).unwrap();
        std::fs::write(ws.path.join("out.txt"), b"work").unwrap();

        daemon.land_agent(agent).unwrap();
        assert!(daemon.workspace_of(agent).is_none(), "agent workspace removed");
        assert!(daemon.get(agent).is_none(), "agent pane removed");
        assert!(branch_exists(repo.path(), "clowder/task-a"), "land keeps the branch");
    }

    #[tokio::test]
    async fn discard_agent_deletes_branch_and_removes_agent() {
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "task-b").unwrap();
        daemon.discard_agent(agent).unwrap();
        assert!(daemon.workspace_of(agent).is_none());
        assert!(daemon.get(agent).is_none());
        assert!(!branch_exists(repo.path(), "clowder/task-b"), "discard deletes the branch");
    }

    fn jj_available() -> bool {
        std::process::Command::new("jj").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// A fresh jj repo with one snapshotted file. Returns the TempDir (kept alive).
    fn init_jj_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("jj").arg("-R").arg(p).args(args)
                .env("JJ_USER", "clowder-test").env("JJ_EMAIL", "clowder@test.invalid")
                .status().unwrap().success();
            assert!(ok, "jj {args:?} failed");
        };
        let ok = std::process::Command::new("jj").args(["git", "init", &p.to_string_lossy()])
            .env("JJ_USER", "clowder-test").env("JJ_EMAIL", "clowder@test.invalid")
            .status().unwrap().success();
        assert!(ok, "jj git init failed");
        std::fs::write(p.join("README.md"), b"init").unwrap();
        run(&["status"]); // force a working-copy snapshot
        dir
    }

    fn jj_bookmark_exists(repo: &std::path::Path, name: &str) -> bool {
        let out = std::process::Command::new("jj").arg("-R").arg(repo).args(["bookmark", "list"])
            .env("JJ_USER", "clowder-test").env("JJ_EMAIL", "clowder@test.invalid")
            .output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains(name)
    }

    #[tokio::test]
    async fn spawn_in_jj_repo_uses_jj_driver_and_land_keeps_bookmark() {
        if !jj_available() { return; }
        use crate::{FakeNotifier, SyntheticAdapter};

        let repo = init_jj_repo();
        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-jj.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let pane = daemon.spawn_agent(repo.path(), &adapter, "task-a").unwrap();
        assert_eq!(daemon.workspace_of(pane).unwrap().kind, clowder_workspace::WorkspaceKind::Jj);
        daemon.land_agent(pane).unwrap();
        assert!(daemon.list_worktrees().is_empty());
        assert!(jj_bookmark_exists(repo.path(), "clowder/task-a"));
    }

    #[tokio::test]
    async fn set_ratio_updates_and_broadcasts() {
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon
            .spawn_agent(
                repo.path(),
                &crate::agent::SyntheticAdapter {
                    command: PaneCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "sleep 30".into()],
                        cwd: None,
                        env: vec![],
                    },
                },
                "t",
            )
            .unwrap();
        let _comp = daemon.split_pane(agent, SplitDirection::Right).unwrap();
        // the split created has id 1 (first split allocated)
        daemon.set_split_ratio(SplitId(1), 0.7).unwrap();
        if let Some(PaneTree::Split { ratio, .. }) = daemon.split_tree_of(agent) {
            assert!((ratio - 0.7).abs() < 1e-6);
        } else {
            panic!("expected a split")
        }
    }

    #[tokio::test]
    async fn companion_crash_removes_leaf_and_broadcasts_tree() {
        use crate::split_tree;
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon
            .spawn_agent(
                repo.path(),
                &crate::agent::SyntheticAdapter {
                    command: PaneCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "sleep 30".into()],
                        cwd: None,
                        env: vec![],
                    },
                },
                "task",
            )
            .unwrap();

        let mut rx = daemon.subscribe_splits();
        let comp = daemon.split_pane(agent, SplitDirection::Right).unwrap();
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()), vec![agent, comp]);
        let _ = rx.try_recv(); // drain the split broadcast

        // Simulate the companion process crashing.
        daemon.get(comp).unwrap().kill().unwrap();

        // The watcher must reap it: tree collapses back to the lone agent leaf, pane gone.
        let mut collapsed = false;
        for _ in 0..100 {
            if daemon.split_tree_of(agent) == Some(PaneTree::Leaf { pane: agent }) {
                collapsed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(collapsed, "a crashed companion's leaf must be removed from the tree");
        assert!(daemon.get(comp).is_none(), "the crashed companion pane must be dropped");

        // A SplitTreeChanged for this agent was broadcast by the reap.
        let mut saw = false;
        for _ in 0..40 {
            match rx.try_recv() {
                Ok((a, _)) if a == agent => {
                    saw = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(saw, "reap must broadcast SplitTreeChanged for the owning agent");

        daemon.teardown_agent(agent).unwrap();
    }

    #[tokio::test]
    async fn reap_companion_is_idempotent() {
        use crate::split_tree;
        let (daemon, repo, _state) = daemon_with_repo();
        let agent = daemon
            .spawn_agent(
                repo.path(),
                &crate::agent::SyntheticAdapter {
                    command: PaneCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "sleep 30".into()],
                        cwd: None,
                        env: vec![],
                    },
                },
                "task",
            )
            .unwrap();
        let comp = daemon.split_pane(agent, SplitDirection::Right).unwrap();

        // Explicit close removes the companion.
        daemon.close_pane(comp).unwrap();
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()), vec![agent]);

        // A late reap (e.g. the watcher firing after close) must be a safe no-op: the tree stays a
        // lone agent leaf and nothing panics.
        daemon.reap_companion(comp);
        assert_eq!(daemon.split_tree_of(agent), Some(PaneTree::Leaf { pane: agent }));

        // Reaping the agent's own leaf must never remove it.
        daemon.reap_companion(agent);
        assert!(daemon.split_tree_of(agent).is_some(), "reap must never remove an agent's own leaf");

        daemon.teardown_agent(agent).unwrap();
    }

    #[tokio::test]
    async fn shutdown_kills_children_and_clears_panes() {
        fn pid_alive(pid: &str) -> bool {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }

        let daemon = Arc::new(Daemon::new());
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!("echo $$ > {}; exec sleep 30", pidfile.display());
        let pane = daemon.spawn_pane(sh(&script), 80, 24).unwrap();

        // Wait for the child to record its PID.
        let mut pid = String::new();
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if !s.trim().is_empty() {
                    pid = s.trim().to_string();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!pid.is_empty(), "child never wrote its PID");
        assert!(pid_alive(&pid), "child alive before shutdown");

        daemon.shutdown();

        // The pane is removed and its child is killed.
        assert!(daemon.get(pane).is_none(), "shutdown must clear the panes map");
        let mut dead = false;
        for _ in 0..100 {
            if !pid_alive(&pid) {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(dead, "shutdown must kill the child PTY process");
    }

    #[tokio::test]
    async fn ratio_change_is_persisted_by_flush() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;
        use clowder_proto::{PaneTree, SplitDirection};

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let state_path = statedir.path().join("agents.json");
        let _state_lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", &state_path);

        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-ratio.sock"),
            state_path.clone(),
            statedir.path().join("projects.json"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
                cwd: None, env: vec![],
            },
        };
        let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();
        daemon.split_pane(id, SplitDirection::Right).unwrap();

        // Find the split id, move its divider, then flush explicitly (no wall-clock dependence).
        let sid = match daemon.split_tree_of(id).unwrap() {
            PaneTree::Split { id, .. } => id,
            _ => panic!("expected split"),
        };
        daemon.set_split_ratio(sid, 0.3).unwrap();
        daemon.flush_dirty_layouts();

        let recs = crate::registry::Registry::new(state_path.clone()).load();
        let tree = recs.iter().find(|r| r.agent_id == id.0).unwrap().tree.clone().unwrap();
        match tree {
            PaneTree::Split { ratio, .. } => assert!((ratio - 0.3).abs() < 1e-6, "ratio persisted: {ratio}"),
            _ => panic!("expected split"),
        }

        daemon.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    #[ignore = "load-sensitive: spawns real /bin/sh + git worktrees; run via `cargo test -- --ignored --test-threads=1`"]
    async fn reconcile_restored_companion_ids_never_collide_with_agents() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;
        use clowder_proto::SplitDirection;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let _state_lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));
        let state_path = statedir.path().join("agents.json");
        let projects_path = statedir.path().join("projects.json");

        let d1 = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-collide1.sock"),
            state_path.clone(),
            projects_path.clone(),
        ));
        d1.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
                cwd: None, env: vec![],
            },
        };
        // Agent A (low id) with a companion, then agent B (higher id).
        let a = d1.spawn_agent(repo.path(), &adapter, "aaa").unwrap();
        d1.split_pane(a, SplitDirection::Right).unwrap();
        let b = d1.spawn_agent(repo.path(), &adapter, "bbb").unwrap();

        // Fresh daemon reconciles A (with layout) then B. Without the early next_id bump, A's
        // restored companion could grab B's id.
        let d2 = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-collide2.sock"),
            state_path,
            projects_path,
        ));
        d2.reconcile();

        // Both agents came back under their original ids.
        let ids: std::collections::HashSet<_> = d2.list_worktrees().iter().map(|x| x.pane).collect();
        assert!(ids.contains(&a) && ids.contains(&b), "both agents restored: {ids:?}");

        // A's companion leaf id differs from BOTH agent ids.
        let tree = d2.split_tree_of(a).unwrap();
        let comp = crate::split_tree::leaves(&tree).into_iter().find(|p| *p != a).unwrap();
        assert_ne!(comp, a, "companion != agent A");
        assert_ne!(comp, b, "companion must not collide with agent B");

        d2.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    async fn reconcile_m9a_record_without_tree_restores_single_leaf() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let _state_lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));
        let state_path = statedir.path().join("agents.json");
        let projects_path = statedir.path().join("projects.json");

        let d1 = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-nolt1.sock"),
            state_path.clone(),
            projects_path.clone(),
        ));
        d1.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
                cwd: None, env: vec![],
            },
        };
        // A plain agent, never split → its record's tree is None (the M9a shape).
        let id = d1.spawn_agent(repo.path(), &adapter, "demo").unwrap();

        let d2 = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-nolt2.sock"),
            state_path,
            projects_path,
        ));
        d2.reconcile();
        assert_eq!(d2.split_tree_of(id), Some(clowder_proto::PaneTree::Leaf { pane: id }));
        d2.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    async fn restart_revives_an_exited_agent_under_the_same_pane_id() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        // An agent that exits immediately.
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "exit 0".into()], cwd: None, env: vec![] } };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();

        // Wait for the exit watcher to mark it Exited.
        for _ in 0..100 {
            if d.attention_of(pane) == Some(AttentionState::Exited) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(d.attention_of(pane), Some(AttentionState::Exited), "agent should have exited");

        d.restart_worktree(pane).unwrap();
        assert_eq!(d.attention_of(pane), Some(AttentionState::Working), "restart resets attention");
        let listed = d.list_worktrees();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].pane, pane, "restart must reuse the pane id — it is the worktree identity");
        d.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn restart_is_refused_while_the_agent_is_alive() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();
        let e = d.restart_worktree(pane).unwrap_err().to_string();
        assert!(e.contains("still running"), "unhelpful message: {e}");
        d.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn restart_of_an_unknown_pane_errors() {
        let state = tempfile::tempdir().unwrap();
        let d = test_daemon_in(state.path());
        let e = d.restart_worktree(PaneId(999)).unwrap_err().to_string();
        assert!(e.contains("no worktree with pane 999"), "should name the missing pane, not claim it's alive: {e}");
    }

    #[tokio::test]
    async fn restart_preserves_a_live_companion_pane_without_duplicating_it() {
        use crate::SyntheticAdapter;
        use clowder_proto::SplitDirection;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        // An agent that exits immediately; its companion (a plain shell) stays alive.
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "exit 0".into()], cwd: None, env: vec![] } };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();
        let companion = d.split_pane(pane, SplitDirection::Right).unwrap();

        // Wait for the agent's own process to exit.
        for _ in 0..100 {
            if d.attention_of(pane) == Some(AttentionState::Exited) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(d.attention_of(pane), Some(AttentionState::Exited), "agent should have exited");

        let panes_before = d.panes.lock().len();

        d.restart_worktree(pane).unwrap();

        let tree = d.split_tree_of(pane).expect("tree restored");
        let leaves = crate::split_tree::leaves(&tree);
        assert_eq!(leaves.len(), 2, "tree still has exactly agent + companion");
        assert!(leaves.contains(&companion), "companion pane id unchanged — reused, not respawned");

        let panes_after = d.panes.lock().len();
        assert_eq!(panes_after, panes_before, "no extra pane created — the original companion was reused, not duplicated");

        d.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn hookless_prompt_sets_needs_input_after_idle() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;
        use std::time::Duration;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let _lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));

        let mut d = Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            "/tmp/unused-vt1.sock".into(),
            statedir.path().join("agents.json"),
            statedir.path().join("projects.json"),
        );
        d.content_idle = Duration::from_millis(40);
        let daemon = Arc::new(d);
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "printf 'Continue? (y/n) '; sleep 30".into()],
                cwd: None, env: vec![],
            },
        };
        let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();

        // Poll up to ~3s for the content-attention escalation.
        let mut got = false;
        for _ in 0..150 {
            if daemon.attention_of(id) == Some(AttentionState::NeedsInput) { got = true; break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(got, "blocking prompt should escalate to NeedsInput");

        daemon.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    /// Spawn a hookless agent running `script` under /bin/sh with a short content-idle. Returns the
    /// daemon, the agent id, and guards (tempdirs + the env lock) the caller must keep alive.
    async fn spawn_hookless(
        script: &str,
    ) -> (Arc<Daemon>, PaneId, tempfile::TempDir, tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        use crate::{FakeNotifier, SyntheticAdapter};
        use std::process::Command as PCommand;
        use std::time::Duration;

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let statedir = tempfile::tempdir().unwrap();
        let lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));

        let mut d = Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            "/tmp/unused-vt.sock".into(),
            statedir.path().join("agents.json"),
            statedir.path().join("projects.json"),
        );
        d.content_idle = Duration::from_millis(40);
        let daemon = Arc::new(d);
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: None, env: vec![],
            },
        };
        let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();
        (daemon, id, repo, statedir, lock)
    }

    async fn wait_for(daemon: &Daemon, id: PaneId, want: AttentionState, ticks: u32) -> bool {
        for _ in 0..ticks {
            if daemon.attention_of(id) == Some(want) { return true; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        daemon.attention_of(id) == Some(want)
    }

    #[tokio::test]
    async fn bare_shell_prompt_does_not_escalate() {
        let (daemon, id, _r, _s, _lock) = spawn_hookless("printf '$ '; sleep 30").await;
        // Give the idle timer several windows to (not) fire.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_ne!(daemon.attention_of(id), Some(AttentionState::NeedsInput),
            "a bare shell prompt must not read as NeedsInput");
        daemon.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    async fn alt_screen_prompt_is_suppressed() {
        // Enter alt-screen, then draw a (y/n): content-attention must be suppressed.
        let (daemon, id, _r, _s, _lock) =
            spawn_hookless("printf '\\033[?1049hContinue? (y/n) '; sleep 30").await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_ne!(daemon.attention_of(id), Some(AttentionState::NeedsInput),
            "a prompt inside the alternate screen must be suppressed");
        daemon.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    #[tokio::test]
    async fn bell_still_escalates_immediately() {
        let (daemon, id, _r, _s, _lock) = spawn_hookless("printf '\\007'; sleep 30").await;
        assert!(wait_for(&daemon, id, AttentionState::NeedsInput, 150).await,
            "BEL must still escalate to NeedsInput");
        daemon.shutdown();
        std::env::remove_var("CLOWDER_STATE_FILE");
    }

    /// A daemon whose registry AND project store live in `dir` — no env vars, no global lock.
    fn test_daemon_in(dir: &std::path::Path) -> Arc<Daemon> {
        Arc::new(Daemon::new_with_paths(
            Arc::new(crate::FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-m10b.sock"),
            dir.join("agents.json"),
            dir.join("projects.json"),
        ))
    }

    #[tokio::test]
    async fn add_and_list_projects_round_trips() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        let rec = d.add_project(repo.path()).unwrap();
        assert_eq!(rec.path, repo.path().canonicalize().unwrap());
        assert_eq!(rec.kind, "git");
        assert_eq!(d.list_projects().len(), 1);
        assert!(d.is_registered_project(repo.path()), "uncanonical path must still match");
    }

    #[tokio::test]
    async fn remove_project_is_refused_while_a_worktree_exists() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();

        let e = d.remove_project(repo.path()).unwrap_err().to_string();
        assert!(e.contains("1"), "message should say how many: {e}");
        assert_eq!(d.list_projects().len(), 1, "project must survive a refused removal");

        d.discard_agent(pane).unwrap();
        d.remove_project(repo.path()).unwrap();      // now allowed
        assert!(d.list_projects().is_empty());
    }

    #[tokio::test]
    async fn remove_project_cannot_race_a_spawn_into_it() {
        use crate::SyntheticAdapter;
        use std::sync::Arc as StdArc;
        let state = tempfile::tempdir().unwrap();
        let repo = crate::test_support::init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();

        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };

        // Spawn on a blocking thread while the main task hammers remove_project. Without the
        // serialization, remove_project can observe an empty `agents` map during the provisioning
        // window and drop the project out from under a live agent.
        //
        // spawn_agent's hookless-scanner setup (finalize_agent) does a bare `tokio::spawn`, which
        // needs ambient runtime context. A plain `std::thread` has none, so enter the current
        // Handle here — otherwise the thread panics with "no reactor running" before the race
        // between remove_project and spawn_agent is ever exercised.
        let rt = tokio::runtime::Handle::current();
        let d2 = StdArc::clone(&d);
        let path = repo.path().to_path_buf();
        let spawner = std::thread::spawn(move || {
            let _guard = rt.enter();
            d2.spawn_agent(&path, &adapter, "racy")
        });

        let mut removed_while_spawning = false;
        for _ in 0..200 {
            if d.remove_project(repo.path()).is_ok() {
                removed_while_spawning = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pane = spawner.join().unwrap();

        if let Ok(pane) = pane {
            assert!(!removed_while_spawning,
                    "removed the project while an agent was being spawned into it");
            assert!(d.is_registered_project(repo.path()), "project must still be registered");
            d.teardown_agent(pane).unwrap();
        } else {
            // The spawn lost the race and was rejected — acceptable, but then the project must
            // genuinely be gone, not half-removed.
            assert!(!d.is_registered_project(repo.path()));
        }
    }

    #[tokio::test]
    async fn project_changes_broadcast_to_subscribers() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        let mut rx = d.subscribe_projects();
        d.add_project(repo.path()).unwrap();
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(crate::server::ProjectChange::Added(rec))) => {
                assert_eq!(rec.path, repo.path().canonicalize().unwrap());
            }
            other => panic!("expected Added, got {other:?}"),
        }
        d.remove_project(repo.path()).unwrap();
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(crate::server::ProjectChange::Removed(p))) => {
                assert_eq!(p, repo.path().canonicalize().unwrap());
            }
            other => panic!("expected Removed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_rejects_an_unregistered_project_and_leaves_nothing_behind() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };

        let e = d.spawn_agent(repo.path(), &adapter, "feat").unwrap_err().to_string();
        assert!(e.contains("unknown project"), "unhelpful message: {e}");
        assert!(!repo.path().join(".clowder/worktrees/feat").exists(), "must not leave a worktree");
        let branches = std::process::Command::new("git").arg("-C").arg(repo.path())
            .args(["branch", "--list", "clowder/feat"]).output().unwrap();
        assert!(branches.stdout.is_empty(), "must not leave a branch");
        assert!(d.list_worktrees().is_empty());
    }

    #[tokio::test]
    async fn spawn_rejects_an_invalid_name_before_provisioning() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };
        let e = d.spawn_agent(repo.path(), &adapter, "my feature").unwrap_err().to_string();
        assert!(e.contains("letters"), "should be the name-validation message: {e}");
        assert!(!repo.path().join(".clowder/worktrees").exists());
    }

    #[tokio::test]
    async fn spawn_rejects_a_colliding_worktree_with_a_real_message() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        // Simulate reconcile's orphan: a worktree dir on disk that the daemon knows nothing about.
        std::fs::create_dir_all(repo.path().join(".clowder/worktrees/feat")).unwrap();
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };
        let e = d.spawn_agent(repo.path(), &adapter, "feat").unwrap_err().to_string();
        assert!(e.contains("already exists"), "should name the collision, not a raw git error: {e}");
        assert!(!e.contains("fatal:"), "must not surface a raw git error: {e}");
    }

    #[tokio::test]
    async fn spawn_stores_the_canonical_project_path() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();
        let listed = d.list_worktrees();
        assert_eq!(listed[0].project, repo.path().canonicalize().unwrap().to_string_lossy());
        d.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn open_project_terminal_is_idempotent() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let a = d.open_project_terminal(repo.path()).unwrap();
        let b = d.open_project_terminal(repo.path()).unwrap();
        assert_eq!(a, b, "a second select must attach to the same shell");
        assert!(d.list_worktrees().is_empty(), "a terminal is not a worktree");
    }

    #[tokio::test]
    async fn forget_project_terminal_does_not_clobber_a_different_live_winner() {
        // Regression test for the `project_terms`/`term_project` bijection race: a losing
        // racer's cleanup must not delete a different, live pane's mapping for the same project.
        // A true concurrency repro is awkward (the race window is inside `spawn_pane`'s fork), so
        // instead we assert the invariant `forget_project_terminal` is now supposed to preserve:
        // simulate the losing racer directly by seeding a stale `term_project` entry for the same
        // project and forgetting it, then check the live winner's mapping survived.
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let root = repo.path().canonicalize().unwrap();
        let live = d.open_project_terminal(repo.path()).unwrap();

        let stale = PaneId(live.0 + 1_000_000);
        d.term_project.lock().insert(stale, root.clone());
        d.forget_project_terminal(stale);

        assert_eq!(
            d.project_terms.lock().get(&root).copied(),
            Some(live),
            "a stale racer's cleanup must not clear the live winner's mapping"
        );
        assert_eq!(
            d.open_project_terminal(repo.path()).unwrap(),
            live,
            "the project's terminal must still resolve to the live pane"
        );
    }

    #[tokio::test]
    async fn project_terminal_can_be_split() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let term = d.open_project_terminal(repo.path()).unwrap();
        let companion = d.split_pane(term, clowder_proto::SplitDirection::Right).unwrap();
        let tree = d.trees.lock().get(&term).cloned().unwrap();
        assert_eq!(crate::split_tree::leaves(&tree).len(), 2);
        assert_eq!(d.owner_of(companion), Some(term));
    }

    #[tokio::test]
    async fn land_and_discard_refuse_a_project_terminal() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let term = d.open_project_terminal(repo.path()).unwrap();
        // finish_agent tolerates a missing workspace, so without a guard these would silently
        // succeed and kill the terminal.
        assert!(d.land_agent(term).is_err(), "land must refuse a project terminal");
        assert!(d.discard_agent(term).is_err(), "discard must refuse a project terminal");
        assert!(d.get(term).is_some(), "the terminal must still be alive");
    }

    #[tokio::test]
    async fn removing_a_project_kills_its_terminal() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let term = d.open_project_terminal(repo.path()).unwrap();
        d.remove_project(repo.path()).unwrap();
        assert!(d.get(term).is_none(), "the terminal pane must be gone");
        assert!(d.project_of_terminal(term).is_none());
    }

    #[tokio::test]
    async fn open_project_terminal_rejects_an_unregistered_project() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        assert!(d.open_project_terminal(repo.path()).is_err());
    }

    // Not one of the brief's five tests — added because `finish_agent`'s companion cascade is
    // the established pattern for tearing down a root+companions unit in this file, and a
    // previous task in this branch shipped a leak by not reusing it. This pins that
    // `forget_project_terminal` (reached by close_pane, remove_project, and natural exit alike)
    // does the same for a split project terminal, instead of orphaning its companion.
    #[tokio::test]
    async fn closing_a_project_terminal_kills_its_companion() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let term = d.open_project_terminal(repo.path()).unwrap();
        let companion = d.split_pane(term, clowder_proto::SplitDirection::Right).unwrap();
        assert!(d.get(companion).is_some(), "sanity: the companion must be alive before closing");

        d.close_pane(term).unwrap();

        assert!(d.get(term).is_none(), "the terminal root must be gone");
        assert!(d.get(companion).is_none(), "closing the root must not orphan its companion pane");
        assert!(d.owner_of(companion).is_none(), "the companion's owner entry must not survive its pane");
    }
}
