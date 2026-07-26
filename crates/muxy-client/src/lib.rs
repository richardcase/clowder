use anyhow::Result;
use muxy_proto::{ClientToDaemon, DaemonToClient, MsgStream, PaneId};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn pump<S, R, W>(io: S, pane: PaneId, mut input: R, mut output: W) -> Result<()>
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
            msg = msgs.recv::<DaemonToClient>() => {
                match msg? {
                    Some(DaemonToClient::Output { bytes, .. }) => {
                        output.write_all(&bytes).await?;
                        output.flush().await?;
                    }
                    Some(DaemonToClient::PaneExited { .. }) | None => break,
                    Some(DaemonToClient::Attached { .. }) => {}
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxy_daemon::{Daemon, PaneCommand};
    use std::sync::Arc;
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
    async fn pump_forwards_input_and_renders_output() {
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("cat"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        // The client end may be dropped by the timeout below before a graceful
        // Detach; that surfaces as a broken-pipe Err here, which is expected.
        tokio::spawn(async move { let _ = d.handle_conn(server_io).await; });

        // Feed "hello\n" via a duplex pipe rather than `std::io::Cursor`: a
        // Cursor reports EOF on the very next poll (always Ready), so pump's
        // `input.read` branch would win the `select!` race and send Detach
        // (closing the socket) before the daemon's echoed `cat` output ever
        // arrives. Keeping the write half alive holds the read half open so
        // `input.read` parks between polls, giving the real client<->daemon
        // round trip time to land. Capture rendered output into a Vec via a
        // recoverable Cursor writer, exactly as originally intended.
        let (mut input_tx, input_rx) = tokio::io::duplex(64);
        input_tx.write_all(b"hello\n").await.unwrap();

        let output = Vec::new();
        // Wrap output so we can read it back after pump returns.
        let output = std::io::Cursor::new(output);

        // Run pump with a timeout so the test can't hang.
        let handle = tokio::spawn(async move {
            let mut out = output;
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                pump(client_io, pane, input_rx, &mut out),
            )
            .await;
            drop(input_tx);
            out.into_inner()
        });

        let rendered = handle.await.unwrap();
        assert!(
            rendered.windows(5).any(|w| w == b"hello"),
            "pump did not render echoed output: {:?}",
            String::from_utf8_lossy(&rendered)
        );
    }
}
