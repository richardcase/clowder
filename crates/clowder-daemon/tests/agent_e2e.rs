use clowder_daemon::server::Daemon;
use clowder_daemon::{FakeNotifier, PaneCommand, SyntheticAdapter};
use clowder_proto::{AttentionState, HookEvent, HookKind, MsgStream, PaneId};
use clowder_workspace::WorktreeLayout;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let run = |args: &[&str]| {
        assert!(Command::new("git").arg("-C").arg(p).args(args).status().unwrap().success());
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
async fn provision_spawn_hook_teardown_end_to_end() {
    let repo = init_repo();
    let hook_dir = tempfile::tempdir().unwrap();
    let hook_sock = hook_dir.path().join("hook.sock");

    let state = tempfile::tempdir().unwrap();
    let notifier = Arc::new(FakeNotifier::new());
    let daemon = Arc::new(Daemon::new_with_paths(
        notifier.clone(),
        hook_sock.clone(),
        state.path().join("agents.json"),
        state.path().join("projects.json"),
        state.path().join("worktrees"),
    ));
    daemon.add_project(repo.path()).unwrap();

    // Serve the hook socket so a simulated agent (below) can post a HookEvent.
    let hook_listener = tokio::net::UnixListener::bind(&hook_sock).unwrap();
    let hooks = daemon.clone();
    tokio::spawn(async move { let _ = hooks.serve_hooks(hook_listener).await; });

    // Synthetic agent = a benign long-lived command that runs in the worktree.
    let adapter = SyntheticAdapter {
        command: PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
            cwd: None,
            env: vec![],
        },
    };
    let pane = daemon.spawn_agent(repo.path(), &adapter, "task-e2e").unwrap();

    // Worktree created on a fresh branch, agent's marker + cwd isolation present.
    let ws_path = WorktreeLayout::new(state.path().join("worktrees"))
        .worktree_path(&repo.path().canonicalize().unwrap(), "task-e2e");
    assert!(ws_path.is_dir(), "worktree not provisioned");
    assert!(ws_path.join(".clowder-agent").is_file(), "adapter hook-provision marker missing");
    assert_eq!(daemon.attention_of(pane), Some(AttentionState::Working));

    // Simulate the agent's hook firing (what clowder-hook would send over CLOWDER_HOOK_SOCK).
    let mut att_rx = daemon.subscribe_attention();
    let stream = tokio::net::UnixStream::connect(&hook_sock).await.unwrap();
    let mut msgs = MsgStream::new(stream);
    msgs.send(&HookEvent { agent_id: pane, kind: HookKind::Notification }).await.unwrap();

    // Attention flips to NeedsInput; broadcast + notifier observe it.
    let mut saw = None;
    for _ in 0..40 {
        if let Ok(Ok((p, s))) = tokio::time::timeout(Duration::from_millis(50), att_rx.recv()).await {
            if p == pane && s == AttentionState::NeedsInput { saw = Some(s); break; }
        }
    }
    assert_eq!(saw, Some(AttentionState::NeedsInput));
    assert!(notifier.calls().contains(&(pane, AttentionState::NeedsInput)));

    // Teardown removes the worktree and drops state.
    daemon.teardown_agent(pane).unwrap();
    assert!(!ws_path.exists(), "worktree not removed on teardown");
    assert_eq!(daemon.attention_of(pane), None);
}

#[tokio::test]
async fn spawn_agent_tears_down_worktree_on_launch_failure() {
    let repo = init_repo();
    let hook_dir = tempfile::tempdir().unwrap();
    let hook_sock = hook_dir.path().join("hook.sock");

    let state = tempfile::tempdir().unwrap();
    let notifier = Arc::new(FakeNotifier::new());
    let daemon = Arc::new(Daemon::new_with_paths(
        notifier.clone(),
        hook_sock.clone(),
        state.path().join("agents.json"),
        state.path().join("projects.json"),
        state.path().join("worktrees"),
    ));
    daemon.add_project(repo.path()).unwrap();

    // Adapter launches a binary that doesn't exist, simulating the common first-run case
    // where the agent CLI isn't on PATH. Pane::spawn should fail after the worktree has
    // already been provisioned.
    let adapter = SyntheticAdapter {
        command: PaneCommand {
            program: "/nonexistent/clowder-bogus-xyz".into(),
            args: vec![],
            cwd: None,
            env: vec![],
        },
    };
    let result = daemon.spawn_agent(repo.path(), &adapter, "task-fail");
    assert!(result.is_err(), "spawn_agent should fail when the agent binary is missing");

    let ws_path = WorktreeLayout::new(state.path().join("worktrees"))
        .worktree_path(&repo.path().canonicalize().unwrap(), "task-fail");
    assert!(!ws_path.exists(), "worktree should be torn down after spawn_agent failure");
}
