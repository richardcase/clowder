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

        let pump_task = tokio::spawn(async move {
            pump(client_io, pane, input_reader, out_writer).await
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
}
