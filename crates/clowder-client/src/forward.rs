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

/// Everything needed to reach one remote daemon: where it is, how to authenticate, and how to
/// decide the certificate is really its.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTarget {
    /// The host nickname. Used for logging and (in M11b) the per-host forwarder socket directory.
    pub label: String,
    /// `host:port` — what we actually dial, and the `known_hosts` key on the un-pinned path.
    pub address: String,
    pub token: Option<String>,
    /// Whether to wrap the connection in TLS. Deliberately INDEPENDENT of `token`: the old
    /// `token.is_some() ⇒ TLS` coupling made it impossible to have an authenticated plaintext
    /// tunnel or a TLS host without a token.
    pub tls: bool,
    /// The pinned server-cert fingerprint, when this host has been paired.
    pub fingerprint: Option<String>,
}

impl RemoteTarget {
    /// The trust policy for one dial. A pin wins; otherwise fall back to TOFU against the
    /// shared `remote_known_hosts`, keyed on the dial address so pre-M11 entries keep matching.
    pub fn trust(&self) -> crate::tofu::Trust {
        match &self.fingerprint {
            Some(fp) => crate::tofu::Trust::Pinned(fp.clone()),
            None => crate::tofu::Trust::Tofu {
                host: self.address.clone(),
                known_hosts: crate::tofu::known_hosts_path(),
            },
        }
    }
}

/// Forward one local connection to the remote daemon: dial, send the channel hello, then pipe
/// bytes both ways until either side closes.
pub async fn forward_stream<L>(mut local: L, target: &RemoteTarget, channel: Channel) -> Result<()>
where
    L: AsyncRead + AsyncWrite + Unpin + Send,
{
    let tcp = dial_with_backoff(&target.address).await?;
    let mut remote: Box<dyn RemoteStream> = if target.tls {
        let connector = tokio_rustls::TlsConnector::from(crate::tofu::connector(target.trust()));
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder")
            .map_err(|e| anyhow::anyhow!("server name: {e}"))?;
        Box::new(connector.connect(name, tcp).await?)
    } else {
        Box::new(tcp)
    };
    // A bearer token in cleartext is worse than no token at all, and the daemon ignores it on a
    // plaintext listener anyway (`serve_remote` passes `expected_token: None`). `resolve_target`
    // refuses this combination up front; this is the belt to that pair of braces.
    let token = if target.tls { target.token.as_deref() } else { None };
    write_hello(&mut remote, channel, token).await?;
    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
    Ok(())
}

/// Object-safe alias so the TLS and plaintext streams can share one path.
trait RemoteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> RemoteStream for T {}

/// Bind the local render + control Unix sockets under `dir` and forward every connection to the
/// remote daemon (render → `Channel::Render`, control → `Channel::Control`). Prints the two paths
/// so callers can point CLOWDER_SOCK / CLOWDER_CONTROL_SOCK at them.
pub async fn forward(target: RemoteTarget, dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&dir)?;
    let render_path = dir.join("clowder.sock");
    let control_path = dir.join("clowder-control.sock");
    let _ = std::fs::remove_file(&render_path);
    let _ = std::fs::remove_file(&control_path);

    let render = UnixListener::bind(&render_path)?;
    let control = UnixListener::bind(&control_path)?;
    println!("clowder connect: forwarding to {} ({})", target.label, target.address);
    println!("  export CLOWDER_SOCK={}", render_path.display());
    println!("  export CLOWDER_CONTROL_SOCK={}", control_path.display());

    let accept = |listener: UnixListener, target: RemoteTarget, channel: Channel| async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("clowder connect: accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            let target = target.clone();
            tokio::spawn(async move {
                if let Err(e) = forward_stream(stream, &target, channel).await {
                    eprintln!("clowder connect: {channel:?} connection ended: {e}");
                }
            });
        }
    };

    tokio::select! {
        _ = accept(render, target.clone(), Channel::Render) => Ok(()),
        _ = accept(control, target, Channel::Control) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn plain_target(addr: &str) -> RemoteTarget {
        RemoteTarget {
            label: "test".into(),
            address: addr.into(),
            token: None,
            tls: false,
            fingerprint: None,
        }
    }

    #[test]
    fn trust_is_pinned_when_the_entry_has_a_fingerprint() {
        let mut t = plain_target("h:1");
        t.fingerprint = Some("aa11".into());
        assert!(matches!(t.trust(), crate::tofu::Trust::Pinned(fp) if fp == "aa11"));
    }

    #[test]
    fn trust_falls_back_to_tofu_keyed_on_the_dial_address() {
        // Keyed on the ADDRESS, not the nickname: entries recorded by earlier versions of
        // clowder were written with the dial address, and must keep matching.
        let t = plain_target("studio.tail:7777");
        match t.trust() {
            crate::tofu::Trust::Tofu { host, .. } => assert_eq!(host, "studio.tail:7777"),
            other => panic!("expected Tofu, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_token_is_never_sent_over_plaintext() {
        // Defense in depth: resolve_target refuses this combination (Task 6), but if one ever
        // reaches the wire, the token must not leak in cleartext.
        let (addr, hello_rx) = echo_remote_recording_hello_returning_token().await;
        let (mut client, server) = tokio::io::duplex(4096);
        let mut target = plain_target(&addr.to_string());
        target.token = Some("s3cr3t".into()); // tls stays false
        let fwd = tokio::spawn(async move { forward_stream(server, &target, Channel::Control).await });

        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).await.unwrap();
        let (_channel, token) = hello_rx.await.unwrap();
        assert_eq!(token, None, "the token must not be sent without TLS");

        drop(client);
        let _ = fwd.await;
    }

    #[tokio::test]
    async fn a_token_is_sent_over_tls() {
        // The positive half of `a_token_is_never_sent_over_plaintext`, and the security-critical
        // seam of the whole feature: without this, deleting the token from the hello entirely
        // still passes the suite, because every other assertion is a negative one and `probe`'s
        // TLS test exercises `probe::authenticate`, a different function with its own copy of the
        // condition.
        let (addr, fp, hello_rx) = tls_remote_recording_hello().await;
        let (mut client, server) = tokio::io::duplex(4096);
        let mut target = plain_target(&addr.to_string());
        target.tls = true;
        target.token = Some("s3cr3t".into());
        target.fingerprint = Some(fp); // pinned, so the test never touches the real known_hosts
        let fwd = tokio::spawn(async move { forward_stream(server, &target, Channel::Control).await });

        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping"); // the bytes really did round-trip through TLS
        let (channel, token) = hello_rx.await.unwrap();
        assert_eq!(channel, 1, "Control hello byte");
        assert_eq!(
            token.as_deref(),
            Some("s3cr3t"),
            "the token must reach the daemon over TLS — otherwise no host can ever authenticate"
        );

        drop(client);
        let _ = fwd.await;
    }

    /// A TLS fake remote: a fresh self-signed cert (so no daemon state dir and no env var are
    /// involved), one accepted connection, the hello recorded, then an echo.
    async fn tls_remote_recording_hello(
    ) -> (std::net::SocketAddr, String, tokio::sync::oneshot::Receiver<(u8, Option<String>)>) {
        use std::sync::Arc;
        use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
        use tokio_rustls::rustls::ServerConfig;

        let ck = rcgen::generate_simple_self_signed(vec!["clowder".to_string()]).unwrap();
        let cert_der = ck.cert.der().to_vec();
        let fp = clowder_proto::cert_fingerprint_hex(&cert_der);
        let key_der = ck.key_pair.serialize_der();
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert_der)],
                PrivateKeyDer::Pkcs8(key_der.into()),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut sock = acceptor.accept(tcp).await.unwrap();
            let (channel, token) = clowder_proto::read_hello(&mut sock).await.unwrap();
            let byte = match channel {
                Channel::Control => 1u8,
                Channel::Render => 2u8,
            };
            let _ = tx.send((byte, token));
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
        (addr, fp, rx)
    }

    // A fake remote: reads the full channel hello (channel byte + length-prefixed optional
    // token), records both, then echoes the rest back.
    async fn echo_remote_recording_hello_returning_token(
    ) -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<(u8, Option<String>)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (channel, token) = clowder_proto::read_hello(&mut sock).await.unwrap();
            let byte = match channel {
                Channel::Control => 1u8,
                Channel::Render => 2u8,
            };
            let _ = tx.send((byte, token));
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
        let (addr, hello_rx) = echo_remote_recording_hello_returning_token().await;
        let (mut client, server) = tokio::io::duplex(4096); // client = test side, server = forwarder's local side
        let target = plain_target(&addr.to_string());
        let fwd = tokio::spawn(async move { forward_stream(server, &target, Channel::Control).await });

        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping"); // bytes round-tripped through the remote echo
        assert_eq!(hello_rx.await.unwrap().0, 1); // Control hello byte (Control == 1) reached the remote

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

        let (addr, hello_rx) = echo_remote_recording_hello_returning_token().await;
        let dir = tempfile::tempdir().unwrap();
        let dirpath = dir.path().to_path_buf();
        let host = addr.to_string();

        let srv = tokio::spawn(async move { forward(plain_target(&host), dirpath).await });
        // wait for the control socket to exist
        let ctl = dir.path().join("clowder-control.sock");
        for _ in 0..50 { if ctl.exists() { break; } tokio::time::sleep(Duration::from_millis(20)).await; }

        let mut c = UnixStream::connect(&ctl).await.unwrap();
        c.write_all(b"hi").await.unwrap();
        let mut got = [0u8; 2];
        c.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hi");                   // round-tripped through the remote echo
        assert_eq!(hello_rx.await.unwrap().0, 1);    // the control socket sent a Control hello (== 1)

        srv.abort();
    }
}
