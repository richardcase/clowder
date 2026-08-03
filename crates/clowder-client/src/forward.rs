use anyhow::Result;
use clowder_proto::{write_hello, Channel};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixListener};

/// Connect to `host`, retrying transient failures with bounded exponential backoff
/// (0.5s → cap 8s, up to 6 attempts) before giving up.
pub async fn dial_with_backoff(host: &str) -> Result<TcpStream> {
    let mut delay = Duration::from_millis(500);
    let mut last_err = None;
    for attempt in 0..6 {
        match TcpStream::connect(host).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                last_err = Some(e);
                if attempt < 5 {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(8));
                }
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not connect to remote {host}: {}",
        last_err.unwrap()
    ))
}

/// Forward one local connection to the remote daemon: dial, send the channel hello, then
/// pipe bytes both ways until either side closes. Dials TLS (with TOFU cert verification) when
/// `token` is `Some` — a token implies the operator configured `[remote] tls=true` on the daemon
/// side — and sends the token in the hello; dials plaintext with no token otherwise.
pub async fn forward_stream<L>(
    mut local: L,
    host: &str,
    channel: Channel,
    token: Option<&str>,
) -> Result<()>
where
    L: AsyncRead + AsyncWrite + Unpin + Send,
{
    let tcp = dial_with_backoff(host).await?;
    let mut remote: Box<dyn RemoteStream> = match token {
        Some(_) => {
            let connector = tokio_rustls::TlsConnector::from(crate::tofu::connector(host));
            let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder")
                .map_err(|e| anyhow::anyhow!("server name: {e}"))?;
            Box::new(connector.connect(name, tcp).await?)
        }
        None => Box::new(tcp),
    };
    write_hello(&mut remote, channel, token).await?;
    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
    Ok(())
}

/// Object-safe alias so the TLS and plaintext streams can share one path.
trait RemoteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> RemoteStream for T {}

/// Bind the local render + control Unix sockets under `dir` and forward every connection to the
/// remote daemon at `host` (render → Channel::Render, control → Channel::Control). Prints the two
/// paths so callers can point CLOWDER_SOCK / CLOWDER_CONTROL_SOCK at them.
pub async fn forward(host: String, dir: PathBuf, token: Option<String>) -> Result<()> {
    std::fs::create_dir_all(&dir)?;
    let render_path = dir.join("clowder.sock");
    let control_path = dir.join("clowder-control.sock");
    let _ = std::fs::remove_file(&render_path);
    let _ = std::fs::remove_file(&control_path);

    let render = UnixListener::bind(&render_path)?;
    let control = UnixListener::bind(&control_path)?;
    println!("clowder connect: forwarding to {host}");
    println!("  export CLOWDER_SOCK={}", render_path.display());
    println!("  export CLOWDER_CONTROL_SOCK={}", control_path.display());

    let accept = |listener: UnixListener, host: String, channel: Channel, token: Option<String>| async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("clowder connect: accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            let host = host.clone();
            let token = token.clone();
            tokio::spawn(async move {
                if let Err(e) = forward_stream(stream, &host, channel, token.as_deref()).await {
                    eprintln!("clowder connect: {channel:?} connection ended: {e}");
                }
            });
        }
    };

    tokio::select! {
        _ = accept(render, host.clone(), Channel::Render, token.clone()) => Ok(()),
        _ = accept(control, host, Channel::Control, token) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // A fake remote: reads the full channel hello (channel byte + length-prefixed optional
    // token), records the channel byte, then echoes the rest back.
    async fn echo_remote_recording_hello(
    ) -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<u8>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (channel, _token) = clowder_proto::read_hello(&mut sock).await.unwrap();
            let hello = match channel {
                Channel::Control => 1u8,
                Channel::Render => 2u8,
            };
            let _ = tx.send(hello);
            let mut buf = [0u8; 64];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sock.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn forwards_hello_then_pipes_bytes() {
        let (addr, hello_rx) = echo_remote_recording_hello().await;
        let (mut client, server) = tokio::io::duplex(4096); // client = test side, server = forwarder's local side
        let fwd = tokio::spawn(async move {
            forward_stream(server, &addr.to_string(), Channel::Control, None).await
        });

        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping"); // bytes round-tripped through the remote echo
        assert_eq!(hello_rx.await.unwrap(), 1); // Control hello byte (Control == 1) reached the remote

        drop(client);
        let _ = fwd.await;
    }

    #[tokio::test(start_paused = true)]
    async fn dial_with_backoff_errors_on_dead_host() {
        // 127.0.0.1:1 refuses quickly; assert we surface an error rather than hang forever.
        let r = dial_with_backoff("127.0.0.1:1").await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn control_socket_forwards_with_control_hello() {
        use tokio::net::UnixStream;

        let (addr, hello_rx) = echo_remote_recording_hello().await;
        let dir = tempfile::tempdir().unwrap();
        let dirpath = dir.path().to_path_buf();
        let host = addr.to_string();

        let srv = tokio::spawn(async move { forward(host, dirpath, None).await });
        // wait for the control socket to exist
        let ctl = dir.path().join("clowder-control.sock");
        for _ in 0..50 { if ctl.exists() { break; } tokio::time::sleep(Duration::from_millis(20)).await; }

        let mut c = UnixStream::connect(&ctl).await.unwrap();
        c.write_all(b"hi").await.unwrap();
        let mut got = [0u8; 2];
        c.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hi");                   // round-tripped through the remote echo
        assert_eq!(hello_rx.await.unwrap(), 1);    // the control socket sent a Control hello (== 1)

        srv.abort();
    }
}
