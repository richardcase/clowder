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
}
