use crate::server::Daemon;
use crate::{ClaudeAdapter, PaneCommand, SyntheticAdapter};
use anyhow::{anyhow, Result};
use muxy_proto::{ControlEvent, ControlRequest, PaneId};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

async fn write_event<W: AsyncWrite + Unpin>(wr: &mut W, ev: &ControlEvent) -> Result<()> {
    let mut s = serde_json::to_string(ev)?;
    s.push('\n');
    wr.write_all(s.as_bytes()).await?;
    wr.flush().await?;
    Ok(())
}

impl Daemon {
    /// Accept loop for the JSON-lines control socket.
    pub async fn serve_control_json(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.handle_control_json(stream).await;
            });
        }
    }

    /// One JSON-lines control connection: snapshot AgentList, then stream events
    /// and handle ListAgents/SpawnAgent requests (newline-delimited JSON both ways).
    pub async fn handle_control_json<S>(self: Arc<Self>, stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let (rd, mut wr) = tokio::io::split(stream);
        let mut lines = BufReader::new(rd).lines();
        let mut att_rx = self.subscribe_attention();
        let mut removed_rx = self.subscribe_removed();
        let mut split_rx = self.subscribe_splits();

        write_event(&mut wr, &ControlEvent::AgentList { agents: self.list_agents() }).await?;

        loop {
            tokio::select! {
                line = lines.next_line() => {
                    match line? {
                        Some(l) if l.trim().is_empty() => continue,
                        Some(l) => {
                            let ev = match serde_json::from_str::<ControlRequest>(&l) {
                                Ok(ControlRequest::ListAgents) =>
                                    ControlEvent::AgentList { agents: self.list_agents() },
                                Ok(ControlRequest::SpawnAgent { project, task, adapter }) =>
                                    match self.spawn_from_control(&project, &task, &adapter) {
                                        Ok(pane) => ControlEvent::AgentSpawned { pane },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::SplitPane { pane, direction }) =>
                                    match self.split_pane(pane, direction) {
                                        Ok(companion) => {
                                            match self.owner_of(companion) {
                                                Some(agent) => self.tree_event(agent),
                                                None => ControlEvent::Error { message: "split produced no owner".into() },
                                            }
                                        }
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::ClosePane { pane }) =>
                                    match self.close_pane(pane) {
                                        Ok(Some(agent)) => self.tree_event(agent),
                                        Ok(None) => ControlEvent::AgentRemoved { pane },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::SetSplitRatio { split, ratio }) =>
                                    match self.set_split_ratio(split, ratio) {
                                        Ok(agent) => self.tree_event(agent),
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::GetSplitTree { agent }) => self.tree_event(agent),
                                Ok(ControlRequest::LandAgent { pane }) => match self.land_agent(pane) {
                                    Ok(()) => ControlEvent::AgentRemoved { pane },
                                    Err(e) => ControlEvent::Error { message: e.to_string() },
                                },
                                Ok(ControlRequest::DiscardAgent { pane }) => match self.discard_agent(pane) {
                                    Ok(()) => ControlEvent::AgentRemoved { pane },
                                    Err(e) => ControlEvent::Error { message: e.to_string() },
                                },
                                Err(e) => ControlEvent::Error { message: format!("bad request: {e}") },
                            };
                            write_event(&mut wr, &ev).await?;
                        }
                        None => break, // client disconnected
                    }
                }
                att = att_rx.recv() => {
                    match att {
                        Ok((pane, state)) =>
                            write_event(&mut wr, &ControlEvent::AttentionChanged { pane, state }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                removed = removed_rx.recv() => {
                    match removed {
                        Ok(pane) => write_event(&mut wr, &ControlEvent::AgentRemoved { pane }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                sp = split_rx.recv() => {
                    match sp {
                        Ok((agent, tree)) =>
                            write_event(&mut wr, &ControlEvent::SplitTreeChanged { agent, tree }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
        Ok(())
    }

    fn spawn_from_control(self: &Arc<Self>, project: &str, task: &str, adapter: &str) -> Result<PaneId> {
        let project_path = Path::new(project);
        match adapter {
            "claude" => self.spawn_agent(project_path, &ClaudeAdapter, task),
            "shell" => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let a = SyntheticAdapter {
                    command: PaneCommand { program: shell, args: vec![], cwd: None, env: vec![] },
                };
                self.spawn_agent(project_path, &a, task)
            }
            other => Err(anyhow!("unknown adapter: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeNotifier;
    use muxy_proto::AttentionState;
    use muxy_workspace::GitWorktreeDriver;
    use std::process::Command as PCommand;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(p).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(p.join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        dir
    }

    #[tokio::test]
    async fn control_json_lists_spawns_and_streams() {
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson.sock"),
        ));

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();

        // Initial snapshot: empty AgentList.
        let first = clines.next_line().await.unwrap().unwrap();
        assert!(first.contains(r#""type":"agentList""#), "{first}");

        // Spawn a shell agent (build the request via the typed enum to escape the path safely).
        let req = ControlRequest::SpawnAgent {
            project: repo.path().to_string_lossy().to_string(),
            task: "demo".into(),
            adapter: "shell".into(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        // Read events until AgentSpawned.
        let pane = loop {
            let l = clines.next_line().await.unwrap().unwrap();
            if let Ok(ControlEvent::AgentSpawned { pane }) = serde_json::from_str::<ControlEvent>(&l) {
                break pane;
            }
        };

        // listAgents now includes it.
        cwr.write_all(b"{\"type\":\"listAgents\"}\n").await.unwrap();
        let listed = loop {
            let l = clines.next_line().await.unwrap().unwrap();
            if let Ok(ControlEvent::AgentList { agents }) = serde_json::from_str::<ControlEvent>(&l) {
                if !agents.is_empty() { break agents; }
            }
        };
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].pane, pane);
        assert_eq!(listed[0].task, "demo");

        // An attention change streams as JSON.
        daemon.set_attention(pane, AttentionState::NeedsInput);
        let mut saw = false;
        for _ in 0..40 {
            if let Ok(Ok(Some(l))) =
                tokio::time::timeout(Duration::from_millis(50), clines.next_line()).await
            {
                if let Ok(ControlEvent::AttentionChanged { pane: p, state }) =
                    serde_json::from_str::<ControlEvent>(&l)
                {
                    if p == pane && state == AttentionState::NeedsInput { saw = true; break; }
                }
            }
        }
        assert!(saw, "did not receive attentionChanged over the control JSON stream");

        daemon.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn split_pane_over_control_stream_yields_split_tree_changed() {
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson3.sock"),
        ));

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();

        // Initial snapshot: empty AgentList.
        let first = clines.next_line().await.unwrap().unwrap();
        assert!(first.contains(r#""type":"agentList""#), "{first}");

        // Spawn a shell agent.
        let req = ControlRequest::SpawnAgent {
            project: repo.path().to_string_lossy().to_string(),
            task: "demo".into(),
            adapter: "shell".into(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let agent = loop {
            let l = clines.next_line().await.unwrap().unwrap();
            if let Ok(ControlEvent::AgentSpawned { pane }) = serde_json::from_str::<ControlEvent>(&l) {
                break pane;
            }
        };

        // Send SplitPane and read events until SplitTreeChanged arrives.
        let req = ControlRequest::SplitPane { pane: agent, direction: muxy_proto::SplitDirection::Right };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let tree = loop {
            let l = clines.next_line().await.unwrap().unwrap();
            if let Ok(ControlEvent::SplitTreeChanged { agent: a, tree }) =
                serde_json::from_str::<ControlEvent>(&l)
            {
                if a == agent {
                    break tree;
                }
            }
        };
        assert_eq!(crate::split_tree::leaves(&tree).len(), 2, "{tree:?}");

        daemon.teardown_agent(agent).unwrap();
    }

    #[tokio::test]
    async fn control_json_spawn_unknown_adapter_errors() {
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson2.sock"),
        ));
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();
        let _snapshot = clines.next_line().await.unwrap().unwrap();

        let req = ControlRequest::SpawnAgent {
            project: repo.path().to_string_lossy().to_string(),
            task: "x".into(),
            adapter: "nope".into(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let l = clines.next_line().await.unwrap().unwrap();
        assert!(l.contains(r#""type":"error""#), "expected error event: {l}");
        assert!(daemon.list_agents().is_empty());
    }
}
