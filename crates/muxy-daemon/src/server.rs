use crate::agent::AgentAdapter;
use crate::notify::{Notifier, OsNotifier};
use crate::{Pane, PaneCommand};
use anyhow::Result;
use muxy_proto::AttentionState;
use muxy_proto::{ClientToDaemon, DaemonToClient, MsgStream, PaneId};
use muxy_proto::{PaneTree, SplitDirection, SplitId};
use muxy_workspace::{GitWorktreeDriver, Workspace, WorkspaceDriver};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;
use tokio::sync::broadcast;

struct AgentMeta {
    project: String,
    task: String,
}

/// The command for a companion pane: the login shell, rooted in the worktree, with no hook env.
pub(crate) fn companion_command(shell: String, cwd: std::path::PathBuf) -> PaneCommand {
    PaneCommand { program: shell, args: vec![], cwd: Some(cwd), env: vec![] }
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
    driver: Arc<dyn WorkspaceDriver>,
    notifier: Arc<dyn Notifier>,
    hook_sock: PathBuf,
    trees: Arc<Mutex<HashMap<PaneId, PaneTree>>>, // agent pane -> split tree
    owner: Arc<Mutex<HashMap<PaneId, PaneId>>>,   // any leaf pane -> its agent
    next_split_id: AtomicU64,
    split_tx: broadcast::Sender<(PaneId, PaneTree)>,
    hookless: Arc<Mutex<std::collections::HashSet<PaneId>>>,
    scanners: Arc<Mutex<HashMap<PaneId, tokio::task::JoinHandle<()>>>>,
}

impl Daemon {
    pub fn new() -> Daemon {
        Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(OsNotifier),
            PathBuf::from("/tmp/muxy-hook.sock"),
        )
    }

    pub fn new_with(
        driver: Arc<dyn WorkspaceDriver>,
        notifier: Arc<dyn Notifier>,
        hook_sock: PathBuf,
    ) -> Daemon {
        let (attention_tx, _) = broadcast::channel(256);
        let (removed_tx, _) = broadcast::channel(256);
        let (split_tx, _) = broadcast::channel(256);
        Daemon {
            panes: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            attention: Arc::new(Mutex::new(HashMap::new())),
            attention_tx,
            removed_tx,
            workspaces: Arc::new(Mutex::new(HashMap::new())),
            agents: Arc::new(Mutex::new(HashMap::new())),
            watchers: Arc::new(Mutex::new(HashMap::new())),
            driver,
            notifier,
            hook_sock,
            trees: Arc::new(Mutex::new(HashMap::new())),
            owner: Arc::new(Mutex::new(HashMap::new())),
            next_split_id: AtomicU64::new(1),
            split_tx,
            hookless: Arc::new(Mutex::new(std::collections::HashSet::new())),
            scanners: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn set_attention(&self, pane: PaneId, state: AttentionState) {
        self.attention.lock().unwrap().insert(pane, state);
        let _ = self.attention_tx.send((pane, state));
        self.notifier.notify(pane, state);
    }

    pub fn attention_of(&self, pane: PaneId) -> Option<AttentionState> {
        self.attention.lock().unwrap().get(&pane).copied()
    }

    pub fn subscribe_attention(&self) -> broadcast::Receiver<(PaneId, AttentionState)> {
        self.attention_tx.subscribe()
    }

    pub fn subscribe_removed(&self) -> broadcast::Receiver<PaneId> {
        self.removed_tx.subscribe()
    }

    /// Path the daemon injects into agents as MUXY_HOOK_SOCK.
    pub fn hook_sock(&self) -> &std::path::Path {
        &self.hook_sock
    }

    fn alloc_id(&self) -> PaneId {
        PaneId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn register_pane(&self, id: PaneId, pane: Pane) {
        self.panes.lock().unwrap().insert(id, Arc::new(pane));
    }

    pub fn spawn_pane(&self, cmd: PaneCommand, cols: u16, rows: u16) -> Result<PaneId> {
        let id = self.alloc_id();
        let pane = Pane::spawn(id, cmd, cols, rows)?;
        self.register_pane(id, pane);
        Ok(id)
    }

    /// Provision an isolated worktree, inject the adapter's hooks, and spawn the agent in it.
    pub fn spawn_agent(self: &Arc<Self>, project: &Path, adapter: &dyn AgentAdapter, task: &str) -> Result<PaneId> {
        let id = self.alloc_id();
        let ws = self.driver.provision(project, task)?;

        // If any post-provision step fails (e.g. the agent binary isn't on PATH), tear down
        // the freshly-provisioned worktree/branch instead of leaking it — otherwise a retry
        // with the same task name fails at `git worktree add`.
        let pane = match (|| -> Result<Pane> {
            adapter.provision_hooks(&ws.path, id, &self.hook_sock)?;

            let mut cmd = adapter.launch_command(&ws.path);
            cmd.cwd = Some(ws.path.clone());
            cmd.env.push(("MUXY_AGENT_ID".into(), id.0.to_string()));
            cmd.env.push(("MUXY_HOOK_SOCK".into(), self.hook_sock.to_string_lossy().to_string()));

            Pane::spawn(id, cmd, 80, 24)
        })() {
            Ok(p) => p,
            Err(e) => {
                // Nothing was ever landed here; fully clean up (worktree + freshly-created
                // branch) so a retry with the same task name doesn't collide.
                let _ = self.driver.discard(&ws);
                return Err(e);
            }
        };
        self.register_pane(id, pane);
        self.workspaces.lock().unwrap().insert(id, ws);
        let project_name = project
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| project.to_string_lossy().to_string());
        self.agents.lock().unwrap().insert(
            id,
            AgentMeta { project: project_name, task: task.to_string() },
        );
        self.set_attention(id, AttentionState::Working);

        if !adapter.provides_hooks() {
            self.hookless.lock().unwrap().insert(id);
            if let Some(pane_arc) = self.panes.lock().unwrap().get(&id).cloned() {
                let me = Arc::clone(self);
                let (snapshot, mut rx) = pane_arc.snapshot_and_subscribe();
                let handle = tokio::spawn(async move {
                    let mut scanner = muxy_vt::SignalScanner::new();
                    // Scan output already produced before we subscribed (no lost early signal).
                    if !scanner.feed(&snapshot).is_empty()
                        && me.attention_of(id) != Some(AttentionState::NeedsInput)
                    {
                        me.set_attention(id, AttentionState::NeedsInput);
                    }
                    loop {
                        match rx.recv().await {
                            Ok(chunk) => {
                                if !scanner.feed(&chunk).is_empty()
                                    && me.attention_of(id) != Some(AttentionState::NeedsInput)
                                {
                                    me.set_attention(id, AttentionState::NeedsInput);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break, // pane gone
                        }
                    }
                });
                self.scanners.lock().unwrap().insert(id, handle);
            }
        }

        self.trees.lock().unwrap().insert(id, PaneTree::Leaf { pane: id });
        self.owner.lock().unwrap().insert(id, id);

        if let Some(pane_arc) = self.panes.lock().unwrap().get(&id).cloned() {
            let me = Arc::clone(self);
            let handle = tokio::spawn(async move {
                pane_arc.wait_exit().await;
                me.set_attention(id, AttentionState::Exited);
            });
            self.watchers.lock().unwrap().insert(id, handle);
        }

        Ok(id)
    }

    pub(crate) fn workspace_of(&self, pane: PaneId) -> Option<Workspace> {
        self.workspaces.lock().unwrap().get(&pane).cloned()
    }

    /// Kill the agent's process and finalize its workspace (land or discard); drop all
    /// per-pane state.
    fn finish_agent(&self, pane: PaneId, land: bool) -> Result<()> {
        // Cascade: kill every companion pane in this agent's tree.
        let companions: Vec<PaneId> = self
            .trees
            .lock()
            .unwrap()
            .get(&pane)
            .map(|t| crate::split_tree::leaves(t).into_iter().filter(|p| *p != pane).collect())
            .unwrap_or_default();
        for c in &companions {
            if let Some(p) = self.get(*c) {
                let _ = p.kill();
            }
            self.panes.lock().unwrap().remove(c);
            self.owner.lock().unwrap().remove(c);
        }
        self.trees.lock().unwrap().remove(&pane);
        self.owner.lock().unwrap().remove(&pane);

        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        if let Some(handle) = self.watchers.lock().unwrap().remove(&pane) {
            handle.abort();
        }
        if let Some(h) = self.scanners.lock().unwrap().remove(&pane) {
            h.abort();
        }
        self.hookless.lock().unwrap().remove(&pane);
        if let Some(ws) = self.workspace_of(pane) {
            if land {
                self.driver.land(&ws)?;
            } else {
                self.driver.discard(&ws)?;
            }
        }
        self.workspaces.lock().unwrap().remove(&pane);
        self.panes.lock().unwrap().remove(&pane);
        self.attention.lock().unwrap().remove(&pane);
        self.agents.lock().unwrap().remove(&pane);
        let _ = self.removed_tx.send(pane);
        Ok(())
    }

    /// Kill the agent's process and remove its worktree without keeping the branch.
    pub fn teardown_agent(&self, pane: PaneId) -> Result<()> {
        self.finish_agent(pane, false)
    }

    /// Finalize the agent's work: commit any dirty changes, remove the worktree, keep the branch.
    pub fn land_agent(&self, pane: PaneId) -> Result<()> {
        self.finish_agent(pane, true)
    }

    /// Throw away the agent's work: remove the worktree and delete its branch.
    pub fn discard_agent(&self, pane: PaneId) -> Result<()> {
        self.finish_agent(pane, false)
    }

    pub fn subscribe_splits(&self) -> broadcast::Receiver<(PaneId, PaneTree)> {
        self.split_tx.subscribe()
    }

    pub fn split_tree_of(&self, agent: PaneId) -> Option<PaneTree> {
        self.trees.lock().unwrap().get(&agent).cloned()
    }

    /// SplitTreeChanged for `agent`, or an Error event if it has no tree.
    pub fn tree_event(&self, agent: PaneId) -> muxy_proto::ControlEvent {
        match self.split_tree_of(agent) {
            Some(tree) => muxy_proto::ControlEvent::SplitTreeChanged { agent, tree },
            None => muxy_proto::ControlEvent::Error { message: format!("no split tree for {agent:?}") },
        }
    }

    fn broadcast_tree(&self, agent: PaneId) {
        if let Some(tree) = self.split_tree_of(agent) {
            let _ = self.split_tx.send((agent, tree));
        }
    }

    fn alloc_split_id(&self) -> SplitId {
        SplitId(self.next_split_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Split `target` (a leaf) by spawning a companion shell in its agent's worktree.
    pub fn split_pane(&self, target: PaneId, direction: SplitDirection) -> Result<PaneId> {
        let agent = *self
            .owner
            .lock()
            .unwrap()
            .get(&target)
            .ok_or_else(|| anyhow::anyhow!("unknown pane {target:?}"))?;
        let path = self
            .workspaces
            .lock()
            .unwrap()
            .get(&agent)
            .map(|w| w.path.clone())
            .ok_or_else(|| anyhow::anyhow!("no workspace for agent {agent:?}"))?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let companion = self.spawn_pane(companion_command(shell, path), 80, 24)?;
        let sid = self.alloc_split_id();
        {
            let mut trees = self.trees.lock().unwrap();
            let tree = trees
                .get_mut(&agent)
                .ok_or_else(|| anyhow::anyhow!("no split tree for {agent:?}"))?;
            let ok = crate::split_tree::split_leaf(tree, target, companion, direction, sid);
            debug_assert!(ok, "split_leaf: {target:?} is not a leaf in the tree for {agent:?}");
        }
        self.owner.lock().unwrap().insert(companion, agent);
        self.broadcast_tree(agent);
        Ok(companion)
    }

    /// Close a companion pane (collapsing the tree), or teardown the agent if `pane` is one.
    /// Returns Some(agent) if a companion was closed, None if an agent was torn down.
    pub fn close_pane(&self, pane: PaneId) -> Result<Option<PaneId>> {
        let is_agent = self.trees.lock().unwrap().contains_key(&pane);
        if is_agent {
            self.teardown_agent(pane)?;
            return Ok(None);
        }
        let agent = *self
            .owner
            .lock()
            .unwrap()
            .get(&pane)
            .ok_or_else(|| anyhow::anyhow!("unknown pane {pane:?}"))?;
        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        self.panes.lock().unwrap().remove(&pane);
        if let Some(tree) = self.trees.lock().unwrap().get_mut(&agent) {
            let removed = crate::split_tree::remove_leaf(tree, pane);
            debug_assert!(removed, "remove_leaf: {pane:?} is not in the tree for {agent:?}");
        }
        self.owner.lock().unwrap().remove(&pane);
        self.broadcast_tree(agent);
        Ok(Some(agent))
    }

    /// Move a divider. Returns the owning agent so callers can emit its tree.
    pub fn set_split_ratio(&self, split: SplitId, ratio: f32) -> Result<PaneId> {
        let mut found = None;
        {
            let mut trees = self.trees.lock().unwrap();
            for (agent, tree) in trees.iter_mut() {
                if crate::split_tree::set_ratio(tree, split, ratio) {
                    found = Some(*agent);
                    break;
                }
            }
        }
        let agent = found.ok_or_else(|| anyhow::anyhow!("unknown split {split:?}"))?;
        self.broadcast_tree(agent);
        Ok(agent)
    }

    /// The agent owning any leaf (or the agent itself).
    pub fn owner_of(&self, pane: PaneId) -> Option<PaneId> {
        self.owner.lock().unwrap().get(&pane).copied()
    }

    pub fn list_agents(&self) -> Vec<muxy_proto::AgentInfo> {
        let agents = self.agents.lock().unwrap();
        let attention = self.attention.lock().unwrap();
        let mut out: Vec<muxy_proto::AgentInfo> = agents
            .iter()
            .map(|(pane, meta)| muxy_proto::AgentInfo {
                pane: *pane,
                project: meta.project.clone(),
                task: meta.task.clone(),
                state: attention.get(pane).copied().unwrap_or(muxy_proto::AttentionState::Working),
            })
            .collect();
        out.sort_by(|a, b| (a.project.as_str(), a.pane.0).cmp(&(b.project.as_str(), b.pane.0)));
        out
    }

    fn get(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.lock().unwrap().get(&id).cloned()
    }

    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.handle_conn(stream).await;
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
                Some(ClientToDaemon::ListAgents) => {
                    return self.handle_control(msgs).await;
                }
                Some(_) => continue, // ignore until attached
                None => return Ok(()),
            }
        };

        let (cols, rows) = pane.size();
        msgs.send(&DaemonToClient::Attached { pane: pane.id(), cols, rows }).await?;

        let (snap, mut sub) = pane.snapshot_and_subscribe();
        msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes: snap }).await?;

        let mut att_rx = self.subscribe_attention();

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
                            if self.hookless.lock().unwrap().contains(&pid)
                                && self.attention_of(pid) == Some(AttentionState::NeedsInput)
                            {
                                self.set_attention(pid, AttentionState::Working);
                            }
                        }
                        Some(ClientToDaemon::Resize { cols, rows, .. }) => { let _ = pane.resize(cols, rows); }
                        Some(ClientToDaemon::Detach) | None => break,
                        Some(ClientToDaemon::Attach { .. }) => continue,
                        Some(ClientToDaemon::ListAgents) => continue,
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
        // Snapshot the agent list, then stream every attention change.
        let mut att_rx = self.subscribe_attention();
        let mut removed_rx = self.subscribe_removed();
        msgs.send(&DaemonToClient::AgentList { agents: self.list_agents() }).await?;
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
                        Some(ClientToDaemon::ListAgents) => {
                            // Client asked to refresh the list.
                            msgs.send(&DaemonToClient::AgentList { agents: self.list_agents() }).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
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

        // Let the detached pane run a bit more.
        tokio::time::sleep(Duration::from_millis(400)).await;

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
        use muxy_proto::AttentionState;
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("sleep 5"), 80, 24).unwrap();

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
            if let Ok(Ok(Some(msg))) =
                tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
            {
                if let DaemonToClient::PaneExited { .. } = msg {
                    exited = true;
                    break;
                }
            } else {
                break; // stream closed
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
    async fn list_agents_reports_project_task_and_state() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use muxy_proto::AttentionState;
        use muxy_workspace::GitWorktreeDriver;
        use std::process::Command as PCommand;
        use std::sync::Arc as StdArc;

        // temp git repo
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

        let daemon = StdArc::new(Daemon::new_with(
            StdArc::new(GitWorktreeDriver),
            StdArc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-listagents.sock"),
        ));
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

        let list = daemon.list_agents();
        assert_eq!(list.len(), 1);
        let a = &list[0];
        assert_eq!(a.pane, pane);
        assert_eq!(a.task, "task-a");
        // project display name is the repo dir's basename
        assert_eq!(a.project, repo.path().file_name().unwrap().to_string_lossy());
        assert_eq!(a.state, AttentionState::NeedsInput);

        daemon.teardown_agent(pane).unwrap();
        assert!(daemon.list_agents().is_empty());
    }

    #[tokio::test]
    async fn control_conn_lists_agents_and_streams_attention() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use muxy_proto::AttentionState;
        use muxy_workspace::GitWorktreeDriver;
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

        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-control.sock"),
        ));
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
        client.send(&ClientToDaemon::ListAgents).await.unwrap();

        // First reply is the agent list.
        match client.recv::<DaemonToClient>().await.unwrap().unwrap() {
            DaemonToClient::AgentList { agents } => {
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].pane, pane);
                assert_eq!(agents[0].task, "task-a");
            }
            other => panic!("expected AgentList, got {other:?}"),
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
        use muxy_proto::AttentionState;
        use muxy_workspace::GitWorktreeDriver;
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

        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-reaper.sock"),
        ));
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
        let list = daemon.list_agents();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].state, AttentionState::Exited);

        daemon.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn teardown_of_running_agent_does_not_leave_spurious_exited() {
        use crate::{FakeNotifier, SyntheticAdapter};
        use muxy_workspace::GitWorktreeDriver;
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

        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-teardown-race.sock"),
        ));
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
        use muxy_workspace::GitWorktreeDriver;
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

        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-removed.sock"),
        ));
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
        client.send(&ClientToDaemon::ListAgents).await.unwrap();
        // Drain the initial AgentList.
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

    /// Temp git repo + a daemon wired up the same way the other integration tests build one.
    fn daemon_with_repo() -> (Arc<Daemon>, tempfile::TempDir) {
        use crate::FakeNotifier;
        use muxy_workspace::GitWorktreeDriver;
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

        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-split-tree.sock"),
        ));
        (daemon, repo)
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
        let (daemon, repo) = daemon_with_repo();
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

        let (daemon, repo) = daemon_with_repo();
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
        let (daemon, repo) = daemon_with_repo();
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
        let (daemon, repo) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &HookedTestAdapter { cmd: bell_then_sleep() }, "t").unwrap();
        // give the BEL time to be produced; attention must stay Working (no scanner).
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(daemon.attention_of(agent), Some(AttentionState::Working));
    }

    #[tokio::test]
    async fn input_clears_hookless_needs_input_to_working() {
        let (daemon, repo) = daemon_with_repo();
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

    fn branch_exists(repo: &std::path::Path, name: &str) -> bool {
        let out = std::process::Command::new("git").arg("-C").arg(repo).args(["branch", "--list", name]).output().unwrap();
        !out.stdout.is_empty()
    }

    #[tokio::test]
    async fn land_agent_keeps_branch_and_removes_agent() {
        let (daemon, repo) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "task-a").unwrap();
        // write some work into the worktree
        let ws = daemon.workspace_of(agent).unwrap();
        std::fs::write(ws.path.join("out.txt"), b"work").unwrap();

        daemon.land_agent(agent).unwrap();
        assert!(daemon.workspace_of(agent).is_none(), "agent workspace removed");
        assert!(daemon.get(agent).is_none(), "agent pane removed");
        assert!(branch_exists(repo.path(), "muxy/task-a"), "land keeps the branch");
    }

    #[tokio::test]
    async fn discard_agent_deletes_branch_and_removes_agent() {
        let (daemon, repo) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "task-b").unwrap();
        daemon.discard_agent(agent).unwrap();
        assert!(daemon.workspace_of(agent).is_none());
        assert!(daemon.get(agent).is_none());
        assert!(!branch_exists(repo.path(), "muxy/task-b"), "discard deletes the branch");
    }

    #[tokio::test]
    async fn set_ratio_updates_and_broadcasts() {
        let (daemon, repo) = daemon_with_repo();
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
}
