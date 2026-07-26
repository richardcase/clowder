use crate::agent::AgentAdapter;
use crate::notify::{Notifier, OsNotifier};
use crate::{Pane, PaneCommand};
use anyhow::Result;
use muxy_proto::AttentionState;
use muxy_proto::{ClientToDaemon, DaemonToClient, MsgStream, PaneId};
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

pub struct Daemon {
    panes: Arc<Mutex<HashMap<PaneId, Arc<Pane>>>>,
    next_id: AtomicU64,
    attention: Arc<Mutex<HashMap<PaneId, AttentionState>>>,
    attention_tx: broadcast::Sender<(PaneId, AttentionState)>,
    workspaces: Arc<Mutex<HashMap<PaneId, Workspace>>>,
    agents: Arc<Mutex<HashMap<PaneId, AgentMeta>>>,
    driver: Arc<dyn WorkspaceDriver>,
    notifier: Arc<dyn Notifier>,
    hook_sock: PathBuf,
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
        Daemon {
            panes: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            attention: Arc::new(Mutex::new(HashMap::new())),
            attention_tx,
            workspaces: Arc::new(Mutex::new(HashMap::new())),
            agents: Arc::new(Mutex::new(HashMap::new())),
            driver,
            notifier,
            hook_sock,
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
    pub fn spawn_agent(&self, project: &Path, adapter: &dyn AgentAdapter, task: &str) -> Result<PaneId> {
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
                let _ = self.driver.teardown(&ws);
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
        Ok(id)
    }

    pub(crate) fn workspace_of(&self, pane: PaneId) -> Option<Workspace> {
        self.workspaces.lock().unwrap().get(&pane).cloned()
    }

    /// Kill the agent's process and remove its worktree; drop all per-pane state.
    pub fn teardown_agent(&self, pane: PaneId) -> Result<()> {
        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        if let Some(ws) = self.workspace_of(pane) {
            self.driver.teardown(&ws)?;
        }
        self.workspaces.lock().unwrap().remove(&pane);
        self.panes.lock().unwrap().remove(&pane);
        self.attention.lock().unwrap().remove(&pane);
        self.agents.lock().unwrap().remove(&pane);
        Ok(())
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
                        Some(ClientToDaemon::Input { bytes, .. }) => { let _ = pane.write_input(&bytes); }
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
}
