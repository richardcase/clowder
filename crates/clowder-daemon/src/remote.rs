use crate::server::Daemon;
use anyhow::{anyhow, bail, Result};
use clowder_proto::Channel;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// A remote peer that connects but never sends its channel hello is dropped after this, so a
/// silent client can't park a spawned task forever (slowloris) on the network listener.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct RemoteTls {
    pub acceptor: TlsAcceptor,
    pub token: String,
}

/// Build a rustls ServerConfig (ring provider, single self-signed cert, no client auth) from creds.
pub fn build_remote_tls(creds: &crate::remote_tls::RemoteCreds) -> Result<RemoteTls> {
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut creds.cert_pem.as_bytes()).collect::<Result<_, _>>()?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut creds.key_pem.as_bytes())?
        .ok_or_else(|| anyhow!("no private key in PEM"))?;
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(RemoteTls { acceptor: TlsAcceptor::from(Arc::new(config)), token: creds.token.clone() })
}

impl Daemon {
    /// Accept loop for the opt-in remote TCP listener. Each connection is prefixed
    /// with a one-byte channel hello, then routed to the same per-connection handler
    /// as the local Unix sockets. The hook channel is never exposed here. When `tls` is
    /// `Some`, every accepted connection is TLS-wrapped and its hello token is verified
    /// against the daemon's bearer token before dispatch; `None` serves plaintext
    /// (loopback/tailnet-only deployments per `should_warn_exposed`).
    pub async fn serve_remote(self: Arc<Self>, listener: TcpListener, tls: Option<RemoteTls>) -> Result<()> {
        loop {
            let (tcp, _addr) = match listener.accept().await {
                Ok(v) => v,
                // Survive a transient accept() error instead of terminating the listener.
                Err(e) => {
                    tracing::warn!("remote accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            let me = self.clone();
            match tls.clone() {
                Some(rt) => {
                    tokio::spawn(async move {
                        match rt.acceptor.accept(tcp).await {
                            Ok(stream) => {
                                if let Some(line) = crate::logging::conn_error_line(
                                    "remote",
                                    me.handle_remote_conn(stream, Some(rt.token.as_str())).await,
                                ) {
                                    tracing::warn!("{line}");
                                }
                            }
                            Err(e) => tracing::warn!("remote TLS handshake failed: {e}"),
                        }
                    });
                }
                None => {
                    tokio::spawn(async move {
                        if let Some(line) = crate::logging::conn_error_line(
                            "remote",
                            me.handle_remote_conn(tcp, None).await,
                        ) {
                            tracing::warn!("{line}");
                        }
                    });
                }
            }
        }
    }

    /// Read the channel hello (+ optional token), verify the token when required, then
    /// dispatch to the existing control/render handler.
    async fn handle_remote_conn<S>(self: Arc<Self>, mut stream: S, expected_token: Option<&str>) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let (channel, token) = tokio::time::timeout(HELLO_TIMEOUT, clowder_proto::read_hello(&mut stream))
            .await
            .map_err(|_| anyhow!("timed out waiting for channel hello"))??;
        if let Some(expected) = expected_token {
            let ok = token
                .as_deref()
                .map(|t| clowder_proto::constant_time_eq(t.as_bytes(), expected.as_bytes()))
                .unwrap_or(false);
            if !ok {
                bail!("remote auth failed (bad or missing token)");
            }
        }
        match channel {
            Channel::Control => self.handle_control_json(stream).await,
            Channel::Render => self.handle_conn(stream).await,
        }
    }
}

/// Phase A has no auth, so binding anywhere but loopback or the Tailscale tailnet
/// ranges (v4 CGNAT 100.64.0.0/10, v6 fd7a:115c:a1e0::/48) deserves a startup
/// warning. Returns true = warn.
pub fn should_warn_exposed(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            let is_tailnet = o[0] == 100 && (64..=127).contains(&o[1]); // 100.64.0.0/10
            !(v4.is_loopback() || is_tailnet)
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            let is_tailnet = o[0..6] == [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0]; // fd7a:115c:a1e0::/48
            !(v6.is_loopback() || is_tailnet)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeNotifier;
    use clowder_proto::{write_hello, ClientToDaemon, MsgStream, PaneId};
    use std::path::PathBuf;
    use tokio::io::{AsyncBufReadExt, BufReader};

    fn test_daemon() -> Arc<Daemon> {
        Arc::new(Daemon::new_with(Arc::new(FakeNotifier::new()), PathBuf::from("/tmp/unused-m7a.sock")))
    }

    #[tokio::test]
    async fn control_hello_routes_to_control_handler() {
        let daemon = test_daemon();
        let (client, server) = tokio::io::duplex(4096);
        let h = tokio::spawn(async move { daemon.handle_remote_conn(server, None).await });

        let mut client = client;
        write_hello(&mut client, Channel::Control, None).await.unwrap();
        // The control handler's first action is to emit an AgentList event as a JSON line.
        let (rd, _wr) = tokio::io::split(client);
        let mut lines = BufReader::new(rd).lines();
        // Bound the read so a regression that stops the handler responding fails fast, not hangs CI.
        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("control handler produced no line within 5s")
            .unwrap()
            .unwrap();
        assert!(line.contains("agentList"), "expected agentList event, got: {line}");
        h.abort();
    }

    #[tokio::test]
    async fn render_hello_routes_to_render_handler() {
        let daemon = test_daemon();
        let (client, server) = tokio::io::duplex(4096);
        let h = tokio::spawn(async move { daemon.handle_remote_conn(server, None).await });

        let mut client = client;
        write_hello(&mut client, Channel::Render, None).await.unwrap();
        // Render handler reads Attach first; an unknown pane ends the session with Ok(()).
        let mut msgs = MsgStream::new(client);
        msgs.send(&ClientToDaemon::Attach { pane: PaneId(999_999) }).await.unwrap();
        let res = tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .expect("render handler did not finish within 5s")
            .unwrap();
        assert!(res.is_ok(), "render route returned: {res:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn silent_client_hello_times_out() {
        let daemon = test_daemon();
        let (client, server) = tokio::io::duplex(64);
        let h = tokio::spawn(async move { daemon.handle_remote_conn(server, None).await });
        // Never send the hello; advance past the timeout (paused clock → no real wait).
        tokio::time::advance(HELLO_TIMEOUT + Duration::from_secs(1)).await;
        let res = h.await.unwrap();
        assert!(res.is_err(), "expected hello timeout error, got: {res:?}");
        drop(client); // keep the client end alive until after the timeout fires
    }

    #[tokio::test]
    async fn tls_control_channel_round_trips_with_token() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _g = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());

        let creds = crate::remote_tls::load_or_generate().unwrap();
        let token = creds.token.clone();
        let fp = crate::remote_tls::fingerprint(&creds);
        let tls = build_remote_tls(&creds).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let daemon = test_daemon();
        tokio::spawn(daemon.serve_remote(listener, Some(tls)));

        // Client side: connect with a verifier pinned to the known fingerprint + the token.
        let connector = crate::remote::test_support::connector_pinned(fp);
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder").unwrap();
        let mut stream = connector.connect(name, tcp).await.unwrap();
        clowder_proto::write_hello(&mut stream, clowder_proto::Channel::Control, Some(&token)).await.unwrap();
        // A control client sends a JSON line request; assert we get a JSON line back (handler engaged).
        stream.write_all(b"{\"type\":\"listAgents\"}\n").await.unwrap();
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf)).await.unwrap().unwrap();
        assert!(n >= 1, "control handler responded over TLS");

        std::env::remove_var("XDG_STATE_HOME");
    }

    #[tokio::test]
    async fn tls_wrong_token_is_rejected() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _g = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());
        let creds = crate::remote_tls::load_or_generate().unwrap();
        let fp = crate::remote_tls::fingerprint(&creds);
        let tls = build_remote_tls(&creds).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(test_daemon().serve_remote(listener, Some(tls)));

        let connector = crate::remote::test_support::connector_pinned(fp);
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder").unwrap();
        let mut stream = connector.connect(name, tcp).await.unwrap();
        clowder_proto::write_hello(&mut stream, clowder_proto::Channel::Control, Some("wrong")).await.unwrap();
        stream.write_all(b"{\"type\":\"listAgents\"}\n").await.unwrap();
        // The daemon drops the connection before dispatch → read returns 0 (EOF).
        let mut buf = [0u8; 8];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf)).await.unwrap().unwrap_or(0);
        assert_eq!(n, 0, "wrong token must be rejected with no handler response");
        std::env::remove_var("XDG_STATE_HOME");
    }

    #[test]
    fn exposure_warning_predicate() {
        use std::net::SocketAddr;
        let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
        // loopback and tailnet (100.64/10) are the sanctioned Phase-A binds → no warning
        assert!(!should_warn_exposed(&addr("127.0.0.1:7777")));
        assert!(!should_warn_exposed(&addr("[::1]:7777")));
        assert!(!should_warn_exposed(&addr("100.101.102.103:7777")));
        // anything else (all-interfaces / LAN / public) has no auth in Phase A → warn
        assert!(should_warn_exposed(&addr("0.0.0.0:7777")));
        assert!(should_warn_exposed(&addr("192.168.1.10:7777")));
        // Tailscale IPv6 (fd7a:115c:a1e0::/48) is also a sanctioned tailnet bind → no warning
        assert!(!should_warn_exposed(&addr("[fd7a:115c:a1e0::1]:7777")));
        // non-tailnet global/ULA IPv6 has no auth in Phase A → warn
        assert!(should_warn_exposed(&addr("[2606:4700::1]:7777")));
    }
}

/// Test-only client-side connector pinned to a known certificate fingerprint (TOFU-style),
/// used by this module's TLS round-trip tests and mirrored by the real client (Task 4).
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::ClientConfig;
    use tokio_rustls::rustls::client::danger::{ServerCertVerified, ServerCertVerifier, HandshakeSignatureValid};
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{DigitallySignedStruct, SignatureScheme, Error};

    #[derive(Debug)]
    struct PinnedFp(String);
    impl ServerCertVerifier for PinnedFp {
        fn verify_server_cert(&self, end_entity: &CertificateDer, _i: &[CertificateDer], _n: &ServerName, _o: &[u8], _t: UnixTime) -> Result<ServerCertVerified, Error> {
            if clowder_proto::cert_fingerprint_hex(end_entity) == self.0 {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(Error::General("fingerprint mismatch".into()))
            }
        }
        fn verify_tls12_signature(&self, _m: &[u8], _c: &CertificateDer, _d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> { Ok(HandshakeSignatureValid::assertion()) }
        fn verify_tls13_signature(&self, _m: &[u8], _c: &CertificateDer, _d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> { Ok(HandshakeSignatureValid::assertion()) }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![SignatureScheme::ECDSA_NISTP256_SHA256, SignatureScheme::ED25519, SignatureScheme::RSA_PSS_SHA256, SignatureScheme::RSA_PKCS1_SHA256]
        }
    }

    pub(crate) fn connector_pinned(fp: String) -> TlsConnector {
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions().unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedFp(fp)))
            .with_no_client_auth();
        TlsConnector::from(Arc::new(config))
    }
}
