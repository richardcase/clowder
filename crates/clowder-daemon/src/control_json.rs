use crate::build_adapter;
use crate::server::Daemon;
use anyhow::{anyhow, Result};
use clowder_proto::{ControlEvent, ControlRequest, PaneId};
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
                if let Some(line) = crate::logging::conn_error_line("control", me.handle_control_json(stream).await) {
                    tracing::warn!("{line}");
                }
            });
        }
    }

    /// One JSON-lines control connection: snapshot WorktreeList, then stream events
    /// and handle ListWorktrees/SpawnAgent requests (newline-delimited JSON both ways).
    pub async fn handle_control_json<S>(self: Arc<Self>, stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let (rd, mut wr) = tokio::io::split(stream);
        let mut lines = BufReader::new(rd).lines();
        let mut att_rx = self.subscribe_attention();
        let mut removed_rx = self.subscribe_removed();
        let mut split_rx = self.subscribe_splits();
        let mut proj_rx = self.subscribe_projects();

        write_event(&mut wr, &ControlEvent::WorktreeList { worktrees: self.list_worktrees() }).await?;

        loop {
            tokio::select! {
                line = lines.next_line() => {
                    match line? {
                        Some(l) if l.trim().is_empty() => continue,
                        Some(l) => {
                            let ev = match serde_json::from_str::<ControlRequest>(&l) {
                                Ok(ControlRequest::ListWorktrees) =>
                                    ControlEvent::WorktreeList { worktrees: self.list_worktrees() },
                                Ok(ControlRequest::ListAdapters) =>
                                    ControlEvent::AdapterList { adapters: self.list_adapters() },
                                Ok(ControlRequest::SpawnAgent { project, name, adapter }) =>
                                    match self.spawn_from_control(&project, &name, &adapter) {
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
                                Ok(ControlRequest::ClosePane { pane }) => {
                                    // A project terminal's maps are cleared by `close_pane`, so
                                    // capture its path first — after the call, `project_of_terminal`
                                    // can no longer answer.
                                    let terminal_path = self.project_of_terminal(pane);
                                    match self.close_pane(pane) {
                                        Ok(Some(agent)) => self.tree_event(agent),
                                        Ok(None) => match terminal_path {
                                            Some(path) => ControlEvent::ProjectTerminalClosed {
                                                path: path.to_string_lossy().to_string(),
                                            },
                                            None => ControlEvent::AgentRemoved { pane },
                                        },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    }
                                }
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
                                Ok(ControlRequest::ListProjects) =>
                                    ControlEvent::ProjectList { projects: self.list_projects() },
                                Ok(ControlRequest::AddProject { path }) =>
                                    match self.add_project(Path::new(&path)) {
                                        Ok(rec) => ControlEvent::ProjectAdded { project: crate::server::project_info(rec) },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::RemoveProject { path }) =>
                                    match self.remove_project(Path::new(&path)) {
                                        Ok(()) => ControlEvent::ProjectRemoved { path },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::RestartWorktree { pane }) =>
                                    match self.restart_worktree(pane) {
                                        Ok(()) => self.tree_event(pane),
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::OpenProjectTerminal { path }) =>
                                    match self.open_project_terminal(Path::new(&path)) {
                                        Ok(pane) => ControlEvent::ProjectTerminalOpened { path, pane },
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
                pc = proj_rx.recv() => {
                    match pc {
                        Ok(crate::server::ProjectChange::Added(rec)) =>
                            write_event(&mut wr, &ControlEvent::ProjectAdded {
                                project: crate::server::project_info(rec) }).await?,
                        Ok(crate::server::ProjectChange::Removed(p)) =>
                            write_event(&mut wr, &ControlEvent::ProjectRemoved {
                                path: p.to_string_lossy().to_string() }).await?,
                        Ok(crate::server::ProjectChange::TerminalClosed(p)) =>
                            write_event(&mut wr, &ControlEvent::ProjectTerminalClosed {
                                path: p.to_string_lossy().to_string() }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
        Ok(())
    }

    fn spawn_from_control(self: &Arc<Self>, project: &str, name: &str, adapter: &str) -> Result<PaneId> {
        let project_path = Path::new(project);
        let a = build_adapter(adapter, &self.shell).ok_or_else(|| anyhow!("unknown adapter: {adapter}"))?;
        self.spawn_agent(project_path, a.as_ref(), name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_repo;
    use crate::FakeNotifier;
    use clowder_proto::AttentionState;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn control_json_lists_spawns_and_streams() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        daemon.add_project(repo.path()).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();

        // Initial snapshot: empty WorktreeList.
        let first = clines.next_line().await.unwrap().unwrap();
        assert!(first.contains(r#""type":"worktreeList""#), "{first}");

        // Spawn a shell agent (build the request via the typed enum to escape the path safely).
        let req = ControlRequest::SpawnAgent {
            project: repo.path().to_string_lossy().to_string(),
            name: "demo".into(),
            adapter: "shell".into(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        // Read events until AgentSpawned. Bounded: a regression that makes the daemon reply
        // with Error instead (e.g. a spawn guard rejecting the request) must fail fast, not
        // hang the test suite forever.
        let pane = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let l = clines.next_line().await.unwrap().unwrap();
                if let Ok(ControlEvent::AgentSpawned { pane }) = serde_json::from_str::<ControlEvent>(&l) {
                    break pane;
                }
            }
        })
        .await
        .expect("no AgentSpawned within 5s");

        // listWorktrees now includes it.
        cwr.write_all(b"{\"type\":\"listWorktrees\"}\n").await.unwrap();
        let listed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let l = clines.next_line().await.unwrap().unwrap();
                if let Ok(ControlEvent::WorktreeList { worktrees }) = serde_json::from_str::<ControlEvent>(&l) {
                    if !worktrees.is_empty() { break worktrees; }
                }
            }
        })
        .await
        .expect("no non-empty WorktreeList within 5s");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].pane, pane);
        assert_eq!(listed[0].name, "demo");

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
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson3.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        daemon.add_project(repo.path()).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();

        // Initial snapshot: empty WorktreeList.
        let first = clines.next_line().await.unwrap().unwrap();
        assert!(first.contains(r#""type":"worktreeList""#), "{first}");

        // Spawn a shell agent.
        let req = ControlRequest::SpawnAgent {
            project: repo.path().to_string_lossy().to_string(),
            name: "demo".into(),
            adapter: "shell".into(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        // Bounded: a spawn-guard regression that turns AgentSpawned into an Error must fail
        // fast, not hang the test suite forever.
        let agent = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let l = clines.next_line().await.unwrap().unwrap();
                if let Ok(ControlEvent::AgentSpawned { pane }) = serde_json::from_str::<ControlEvent>(&l) {
                    break pane;
                }
            }
        })
        .await
        .expect("no AgentSpawned within 5s");

        // Send SplitPane and read events until SplitTreeChanged arrives.
        let req = ControlRequest::SplitPane { pane: agent, direction: clowder_proto::SplitDirection::Right };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let tree = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let l = clines.next_line().await.unwrap().unwrap();
                if let Ok(ControlEvent::SplitTreeChanged { agent: a, tree }) =
                    serde_json::from_str::<ControlEvent>(&l)
                {
                    if a == agent {
                        break tree;
                    }
                }
            }
        })
        .await
        .expect("no matching SplitTreeChanged within 5s");
        assert_eq!(crate::split_tree::leaves(&tree).len(), 2, "{tree:?}");

        daemon.teardown_agent(agent).unwrap();
    }

    #[tokio::test]
    async fn close_pane_on_a_project_terminal_replies_project_terminal_closed() {
        // Regression test: closing a project terminal must reply `projectTerminalClosed`, not
        // `agentRemoved` — the terminal was never a worktree, so `agentRemoved` is a lie the
        // client would act on (e.g. by looking for it in a worktree list it was never part of).
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson5.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        daemon.add_project(repo.path()).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();
        let _snapshot = clines.next_line().await.unwrap().unwrap();

        let req = ControlRequest::OpenProjectTerminal {
            path: repo.path().to_string_lossy().to_string(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let pane = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let l = clines.next_line().await.unwrap().unwrap();
                if let Ok(ControlEvent::ProjectTerminalOpened { pane, .. }) =
                    serde_json::from_str::<ControlEvent>(&l)
                {
                    break pane;
                }
            }
        })
        .await
        .expect("no ProjectTerminalOpened within 5s");

        let req = ControlRequest::ClosePane { pane };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let reply = tokio::time::timeout(Duration::from_secs(5), clines.next_line())
            .await
            .expect("no reply to closePane within 5s")
            .unwrap()
            .unwrap();
        assert!(
            reply.contains(r#""type":"projectTerminalClosed""#),
            "expected projectTerminalClosed, got: {reply}"
        );
        assert!(
            !reply.contains(r#""type":"agentRemoved""#),
            "must not reply agentRemoved for a project terminal: {reply}"
        );
        match serde_json::from_str::<ControlEvent>(&reply).unwrap() {
            ControlEvent::ProjectTerminalClosed { path } => {
                assert_eq!(path, repo.path().canonicalize().unwrap().to_string_lossy());
            }
            other => panic!("expected ProjectTerminalClosed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn control_json_spawn_unknown_adapter_errors() {
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with(
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
            name: "x".into(),
            adapter: "nope".into(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let l = clines.next_line().await.unwrap().unwrap();
        assert!(l.contains(r#""type":"error""#), "expected error event: {l}");
        assert!(daemon.list_worktrees().is_empty());
    }

    #[test]
    fn list_adapters_returns_registry_descriptor_ids() {
        let daemon = Daemon::new();
        let ids: Vec<String> = daemon.list_adapters().into_iter().map(|a| a.id).collect();
        // Descriptor ids (NOT adapter.id() — shell's adapter reports "synthetic").
        assert!(ids.contains(&"claude".to_string()));
        assert!(ids.contains(&"codex".to_string()));
        assert!(ids.contains(&"shell".to_string()));
        assert!(!ids.contains(&"synthetic".to_string()), "must expose descriptor id 'shell', not 'synthetic'");
    }

    #[tokio::test]
    async fn control_json_adds_lists_and_streams_projects() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-projects.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });
        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();
        let _snapshot = clines.next_line().await.unwrap().unwrap();

        let req = ControlRequest::AddProject { path: repo.path().to_string_lossy().to_string() };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        // The reply is ProjectAdded, with the kind detected and the name derived. Bounded: a
        // regression that stops the daemon sending ProjectAdded must fail fast, not hang CI.
        let added = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let l = clines.next_line().await.unwrap().unwrap();
                if let Ok(ControlEvent::ProjectAdded { project }) = serde_json::from_str::<ControlEvent>(&l) {
                    break project;
                }
            }
        })
        .await
        .expect("no ProjectAdded within 5s");
        assert_eq!(added.kind, "git");
        assert_eq!(added.path, repo.path().canonicalize().unwrap().to_string_lossy());

        cwr.write_all(b"{\"type\":\"listProjects\"}\n").await.unwrap();
        let listed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let l = clines.next_line().await.unwrap().unwrap();
                if let Ok(ControlEvent::ProjectList { projects }) = serde_json::from_str::<ControlEvent>(&l) {
                    if !projects.is_empty() { break projects; }
                }
            }
        })
        .await
        .expect("no non-empty ProjectList within 5s");
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn control_json_add_project_rejects_a_non_repo() {
        let state = tempfile::tempdir().unwrap();
        let plain = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-projects2.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });
        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();
        let _snapshot = clines.next_line().await.unwrap().unwrap();

        let req = ControlRequest::AddProject { path: plain.path().to_string_lossy().to_string() };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        // Bounded: a regression that stops the daemon replying must fail fast, not hang CI.
        let l = tokio::time::timeout(std::time::Duration::from_secs(5), clines.next_line())
            .await
            .expect("no reply within 5s")
            .unwrap()
            .unwrap();
        assert!(l.contains("not a git or jj repository"), "expected a helpful error: {l}");
        assert!(daemon.list_projects().is_empty());
    }

    #[tokio::test]
    async fn control_json_list_adapters_yields_adapter_list_with_codex() {
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson4.sock"),
        ));

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();

        // Initial snapshot: empty WorktreeList.
        let first = clines.next_line().await.unwrap().unwrap();
        assert!(first.contains(r#""type":"worktreeList""#), "{first}");

        cwr.write_all(b"{\"type\":\"listAdapters\"}\n").await.unwrap();
        let l = clines.next_line().await.unwrap().unwrap();
        assert!(l.contains(r#""type":"adapterList""#), "{l}");
        let adapters = match serde_json::from_str::<ControlEvent>(&l).unwrap() {
            ControlEvent::AdapterList { adapters } => adapters,
            other => panic!("expected AdapterList, got {other:?}"),
        };
        assert!(adapters.iter().any(|a| a.id == "codex"), "{adapters:?}");
    }
}
