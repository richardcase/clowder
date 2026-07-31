use crate::server::Daemon;
use anyhow::Result;
use clowder_proto::{read_hello, Channel};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

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
        match read_hello(&mut stream).await? {
            Channel::Control => self.handle_control_json(stream).await,
            Channel::Render => self.handle_conn(stream).await,
        }
    }
}

use std::net::{IpAddr, SocketAddr};

/// Phase A has no auth, so binding anywhere but loopback or the Tailscale CGNAT
/// range (100.64.0.0/10) deserves a startup warning. Returns true = warn.
pub fn should_warn_exposed(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            let is_tailnet = o[0] == 100 && (64..=127).contains(&o[1]); // 100.64.0.0/10
            !(v4.is_loopback() || is_tailnet)
        }
        IpAddr::V6(v6) => !v6.is_loopback(),
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
        let line = lines.next_line().await.unwrap().unwrap();
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
        let res = h.await.unwrap();
        assert!(res.is_ok(), "render route returned: {res:?}");
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
    }
}
