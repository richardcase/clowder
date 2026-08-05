use anyhow::Result;
use clowder_proto::{ClientToDaemon, ControlEvent, ControlRequest, DaemonToClient, MsgStream, PaneId};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub mod forward;
mod tofu;

/// RAII guard that restores the terminal from raw mode when dropped, even on
/// error paths or panics/unwinds — so a crash in `pump` never leaves the
/// user's terminal wrecked.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(RawModeGuard)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Build a Resize message for the pane (pure; unit-tested).
pub fn resize_msg(pane: PaneId, cols: u16, rows: u16) -> ClientToDaemon {
    ClientToDaemon::Resize { pane, cols, rows }
}

/// Connect to the daemon's raw-mode socket and attach to `pane_id`, pumping
/// stdin/stdout until the pane exits or the terminal is detached.
pub async fn attach(pane_id: u64) -> Result<()> {
    let sock = clowder_config::Config::load().client_sock;
    let pane = PaneId(pane_id);

    let stream = UnixStream::connect(&sock).await?;

    // Put the real terminal in raw mode so keystrokes reach the pane unbuffered.
    let _guard = RawModeGuard::enable()?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    // Resize source: send the current size immediately, then on each SIGWINCH.
    let (tx, rx) = tokio::sync::mpsc::channel::<(u16, u16)>(8);
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let _ = tx.send((cols, rows)).await;
    }
    let winch_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        {
            while sig.recv().await.is_some() {
                if let Ok((cols, rows)) = crossterm::terminal::size() {
                    if winch_tx.send((cols, rows)).await.is_err() {
                        break; // pump gone
                    }
                }
            }
        }
    });

    pump(stream, pane, stdin, stdout, rx).await
    // _guard drops here, restoring raw mode; pump's Result is returned unmasked.
}

/// Connect the JSON control socket, request a spawn, and return the new pane id.
pub async fn spawn_via_control(
    control_sock: &Path,
    project: &str,
    name: &str,
    adapter: &str,
) -> anyhow::Result<PaneId> {
    let stream = UnixStream::connect(control_sock).await?;
    let (rd, mut wr) = tokio::io::split(stream);
    let mut lines = BufReader::new(rd).lines();

    let req = ControlRequest::SpawnAgent {
        project: project.to_string(),
        name: name.to_string(),
        adapter: adapter.to_string(),
    };
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    wr.write_all(line.as_bytes()).await?;

    // Skip the initial WorktreeList / any streamed events until the spawn result.
    loop {
        match lines.next_line().await? {
            Some(l) => match serde_json::from_str::<ControlEvent>(&l) {
                Ok(ControlEvent::AgentSpawned { pane }) => return Ok(pane),
                Ok(ControlEvent::Error { message }) => return Err(anyhow::anyhow!(message)),
                Ok(_) => continue, // WorktreeList / AttentionChanged / AgentRemoved
                Err(_) => continue, // ignore unparseable lines defensively
            },
            None => return Err(anyhow::anyhow!("control socket closed before spawn result")),
        }
    }
}

pub async fn pump<S, R, W>(
    io: S,
    pane: PaneId,
    mut input: R,
    mut output: W,
    mut resizes: tokio::sync::mpsc::Receiver<(u16, u16)>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut msgs = MsgStream::new(io);
    msgs.send(&ClientToDaemon::Attach { pane }).await?;

    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            n = input.read(&mut buf) => {
                match n {
                    Ok(0) => { let _ = msgs.send(&ClientToDaemon::Detach).await; break; }
                    Ok(n) => msgs.send(&ClientToDaemon::Input { pane, bytes: buf[..n].to_vec() }).await?,
                    Err(_) => break,
                }
            }
            Some((cols, rows)) = resizes.recv() => {
                msgs.send(&resize_msg(pane, cols, rows)).await?;
            }
            msg = msgs.recv::<DaemonToClient>() => {
                match msg? {
                    Some(DaemonToClient::Output { bytes, .. }) => {
                        output.write_all(&bytes).await?;
                        output.flush().await?;
                    }
                    Some(DaemonToClient::PaneExited { .. }) | None => break,
                    Some(DaemonToClient::Attached { .. }) => {}
                    Some(DaemonToClient::AttentionChanged { .. }) => {}
                    Some(DaemonToClient::WorktreeList { .. }) => {}
                    Some(DaemonToClient::AgentRemoved { .. }) => {}
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clowder_daemon::{Daemon, PaneCommand};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn resize_msg_builds_resize_variant() {
        let m = resize_msg(PaneId(7), 120, 40);
        assert_eq!(m, ClientToDaemon::Resize { pane: PaneId(7), cols: 120, rows: 40 });
    }

    #[tokio::test]
    async fn pump_forwards_resize_from_channel() {
        use clowder_proto::MsgStream;
        let pane = PaneId(3);
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (tx, rx) = tokio::sync::mpsc::channel::<(u16, u16)>(8);

        // Empty stdin (never yields) and a sink stdout.
        let (input_reader, _input_writer) = tokio::io::duplex(64);
        let (_out_reader, out_writer) = tokio::io::duplex(64);

        let pump_task = tokio::spawn(async move {
            pump(client_io, pane, input_reader, out_writer, rx).await
        });

        tx.send((100, 40)).await.unwrap();

        let mut server = MsgStream::new(server_io);
        // First frame is Attach, then our Resize.
        let first: ClientToDaemon = server.recv().await.unwrap().unwrap();
        assert_eq!(first, ClientToDaemon::Attach { pane });
        let second: ClientToDaemon = server.recv().await.unwrap().unwrap();
        assert_eq!(second, ClientToDaemon::Resize { pane, cols: 100, rows: 40 });

        drop(tx);
        pump_task.abort();
    }

    fn sh(script: &str) -> PaneCommand {
        PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: vec![],
        }
    }

    #[tokio::test]
    async fn pump_forwards_input_renders_output_and_shuts_down_on_eof() {
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("cat"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        let server = tokio::spawn(async move { let _ = d.handle_conn(server_io).await; });

        // pump reads `input_reader`; test writes into `input_writer`.
        let (input_reader, mut input_writer) = tokio::io::duplex(1024);
        // pump writes into `out_writer`; test reads `out_reader`.
        let (mut out_reader, out_writer) = tokio::io::duplex(64 * 1024);

        let (_resize_tx, resize_rx) = tokio::sync::mpsc::channel::<(u16, u16)>(8);
        let pump_task = tokio::spawn(async move {
            pump(client_io, pane, input_reader, out_writer, resize_rx).await
        });

        input_writer.write_all(b"hello\n").await.unwrap();
        input_writer.flush().await.unwrap();

        // Read rendered output until we see the echo (bounded).
        let mut seen = Vec::new();
        let mut buf = [0u8; 1024];
        let got = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let n = out_reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
                if seen.windows(5).any(|w| w == b"hello") {
                    break;
                }
            }
        })
        .await;
        assert!(got.is_ok(), "did not render echoed output in time");
        assert!(
            seen.windows(5).any(|w| w == b"hello"),
            "output missing 'hello': {:?}",
            String::from_utf8_lossy(&seen)
        );

        // Close pump's input -> pump must send Detach and return cleanly & promptly.
        drop(input_writer);
        let pump_result = tokio::time::timeout(Duration::from_secs(5), pump_task).await;
        assert!(pump_result.is_ok(), "pump did not return after input EOF (it hung)");
        assert!(
            pump_result.unwrap().unwrap().is_ok(),
            "pump returned an error on EOF shutdown"
        );

        let _ = server.await;
    }

    #[tokio::test]
    async fn spawn_via_control_returns_pane_id() {
        use clowder_daemon::server::Daemon;
        use clowder_daemon::FakeNotifier;
        use std::process::Command as PCommand;
        use std::sync::Arc;

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

        // daemon + control socket on a temp path
        let sockdir = tempfile::tempdir().unwrap();
        let sock = sockdir.path().join("control.sock");
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cli.sock"),
        ));
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.serve_control_json(listener).await; });

        let pane = spawn_via_control(&sock, &repo.path().to_string_lossy(), "demo", "shell")
            .await
            .unwrap();

        let agents = daemon.list_worktrees();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].pane, pane);
        assert_eq!(agents[0].name, "demo");

        daemon.teardown_agent(pane).unwrap();
    }
}
