use crate::server::Daemon;
use anyhow::{anyhow, Result};
use clowder_proto::{read_hello, Channel};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

/// A remote peer that connects but never sends its channel hello is dropped after this, so a
/// silent client can't park a spawned task forever (slowloris) on the network listener.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

impl Daemon {
    /// Accept loop for the opt-in remote TCP listener. Each connection is prefixed
    /// with a one-byte channel hello, then routed to the same per-connection handler
    /// as the local Unix sockets. The hook channel is never exposed here.
    pub async fn serve_remote(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                if let Some(line) = crate::logging::conn_error_line("remote", me.handle_remote_conn(stream).await) {
                    tracing::warn!("{line}");
                }
            });
        }
    }

    /// Read the channel hello, then dispatch to the existing control/render handler.
    async fn handle_remote_conn<S>(self: Arc<Self>, mut stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let channel = tokio::time::timeout(HELLO_TIMEOUT, read_hello(&mut stream))
            .await
            .map_err(|_| anyhow!("timed out waiting for channel hello"))??;
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
        let h = tokio::spawn(async move { daemon.handle_remote_conn(server).await });

        let mut client = client;
        write_hello(&mut client, Channel::Control).await.unwrap();
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
        let h = tokio::spawn(async move { daemon.handle_remote_conn(server).await });

        let mut client = client;
        write_hello(&mut client, Channel::Render).await.unwrap();
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
        let h = tokio::spawn(async move { daemon.handle_remote_conn(server).await });
        // Never send the hello; advance past the timeout (paused clock → no real wait).
        tokio::time::advance(HELLO_TIMEOUT + Duration::from_secs(1)).await;
        let res = h.await.unwrap();
        assert!(res.is_err(), "expected hello timeout error, got: {res:?}");
        drop(client); // keep the client end alive until after the timeout fires
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
