use crate::{Pane, PaneCommand};
use anyhow::Result;
use muxy_proto::{ClientToDaemon, DaemonToClient, MsgStream, PaneId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;

pub struct Daemon {
    panes: Arc<Mutex<HashMap<PaneId, Arc<Pane>>>>,
    next_id: AtomicU64,
}

impl Daemon {
    pub fn new() -> Daemon {
        Daemon {
            panes: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn spawn_pane(&self, cmd: PaneCommand, cols: u16, rows: u16) -> Result<PaneId> {
        let id = PaneId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let pane = Pane::spawn(id, cmd, cols, rows)?;
        self.panes.lock().unwrap().insert(id, Arc::new(pane));
        Ok(id)
    }

    fn get(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.lock().unwrap().get(&id).cloned()
    }

    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.handle_conn(stream).await;
            });
        }
    }

    pub async fn handle_conn<S>(self: Arc<Self>, stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let mut msgs = MsgStream::new(stream);
        // First message must be Attach.
        let pane = loop {
            match msgs.recv::<ClientToDaemon>().await? {
                Some(ClientToDaemon::Attach { pane }) => match self.get(pane) {
                    Some(p) => break p,
                    None => return Ok(()), // unknown pane: end session
                },
                Some(_) => continue, // ignore until attached
                None => return Ok(()),
            }
        };

        let (cols, rows) = pane.size();
        msgs.send(&DaemonToClient::Attached { pane: pane.id(), cols, rows }).await?;

        let (snap, mut sub) = pane.snapshot_and_subscribe();
        msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes: snap }).await?;

        loop {
            tokio::select! {
                live = sub.recv() => {
                    match live {
                        Ok(bytes) => msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                incoming = msgs.recv::<ClientToDaemon>() => {
                    match incoming? {
                        Some(ClientToDaemon::Input { bytes, .. }) => { let _ = pane.write_input(&bytes); }
                        Some(ClientToDaemon::Resize { cols, rows, .. }) => { let _ = pane.resize(cols, rows); }
                        Some(ClientToDaemon::Detach) | None => break,
                        Some(ClientToDaemon::Attach { .. }) => continue,
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    async fn client_attaches_and_receives_output() {
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("cat"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();

        // Expect Attached, then a (possibly empty) backlog Output.
        let attached: DaemonToClient = client.recv().await.unwrap().unwrap();
        assert!(matches!(attached, DaemonToClient::Attached { .. }));
        let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

        client
            .send(&ClientToDaemon::Input { pane, bytes: b"echo hi\n".to_vec() })
            .await
            .unwrap();

        let mut seen = Vec::new();
        for _ in 0..50 {
            if let Ok(Ok(Some(DaemonToClient::Output { bytes, .. }))) =
                tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
            {
                seen.extend_from_slice(&bytes);
                if seen.windows(2).any(|w| w == b"hi") {
                    break;
                }
            }
        }
        assert!(seen.windows(2).any(|w| w == b"hi"), "did not receive echoed output");
    }

    #[tokio::test]
    async fn pane_survives_detach_and_replays_on_reattach() {
        use std::time::Duration;

        let daemon = Arc::new(Daemon::new());
        // A shell that appends a line every 100ms to prove it keeps running while detached.
        let pane = daemon
            .spawn_pane(sh("i=0; while true; do i=$((i+1)); echo line$i; sleep 0.1; done"), 80, 24)
            .unwrap();

        // First client attaches, collects some output, then detaches.
        {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            let d = daemon.clone();
            let h = tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

            let mut client = MsgStream::<_>::new(client_io);
            client.send(&ClientToDaemon::Attach { pane }).await.unwrap();
            let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
            let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

            // Read a little live output.
            let mut seen = Vec::new();
            for _ in 0..30 {
                if let Ok(Ok(Some(DaemonToClient::Output { bytes, .. }))) =
                    tokio::time::timeout(Duration::from_millis(100), client.recv::<DaemonToClient>()).await
                {
                    seen.extend_from_slice(&bytes);
                    if seen.windows(5).any(|w| w == b"line1") {
                        break;
                    }
                }
            }
            assert!(seen.windows(5).any(|w| w == b"line1"), "first attach saw no output");

            client.send(&ClientToDaemon::Detach).await.unwrap();
            let _ = h.await; // session ends; pane must keep running
        }

        // Let the detached pane run a bit more.
        tokio::time::sleep(Duration::from_millis(400)).await;

        // Second client reattaches; the backlog replay must contain later lines
        // that were produced WHILE no client was attached.
        {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            let d = daemon.clone();
            tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

            let mut client = MsgStream::<_>::new(client_io);
            client.send(&ClientToDaemon::Attach { pane }).await.unwrap();
            let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
            let backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

            let bytes = match backlog {
                DaemonToClient::Output { bytes, .. } => bytes,
                other => panic!("expected backlog Output, got {other:?}"),
            };
            // At least line4+ should exist, proving the pane produced output while detached.
            assert!(
                bytes.windows(5).any(|w| w == b"line4"),
                "reattach backlog did not include output produced while detached: {:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }
}
