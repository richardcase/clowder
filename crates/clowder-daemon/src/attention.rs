// SPDX-License-Identifier: Apache-2.0

use crate::server::Daemon;
use anyhow::Result;
use clowder_proto::{AttentionState, HookEvent, HookKind, MsgStream};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;

impl Daemon {
    /// Accept loop for the hook socket: each connection delivers one HookEvent.
    pub async fn serve_hooks(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                if let Some(line) = crate::logging::conn_error_line("hook", me.handle_hook_conn(stream).await) {
                    tracing::warn!("{line}");
                }
            });
        }
    }

    /// Read one HookEvent from a hook connection and apply it to attention state.
    pub async fn handle_hook_conn<S>(self: Arc<Self>, stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let mut msgs = MsgStream::new(stream);
        if let Some(event) = msgs.recv::<HookEvent>().await? {
            let state = match event.kind {
                HookKind::Notification => AttentionState::NeedsInput,
                HookKind::Stop => AttentionState::Completed,
                HookKind::Active => AttentionState::Working,
            };
            self.set_attention(event.agent_id, state);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::FakeNotifier;
    use clowder_proto::PaneId;
    use std::path::PathBuf;
    use std::time::Duration;

    #[tokio::test]
    async fn hook_event_updates_attention_broadcasts_and_notifies() {
        let notifier = Arc::new(FakeNotifier::new());
        let daemon = Arc::new(Daemon::new_with(
            notifier.clone(),
            PathBuf::from("/tmp/unused.sock"),
        ));

        let mut att_rx = daemon.subscribe_attention();

        // Drive one hook connection over an in-memory duplex (stands in for the socket).
        let (client_io, server_io) = tokio::io::duplex(4096);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_hook_conn(server_io).await.unwrap() });

        let mut client = MsgStream::new(client_io);
        client
            .send(&HookEvent { agent_id: PaneId(7), kind: HookKind::Notification })
            .await
            .unwrap();

        // Broadcast observed.
        let (pane, state) = tokio::time::timeout(Duration::from_secs(2), att_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((pane, state), (PaneId(7), AttentionState::NeedsInput));
        // State stored.
        assert_eq!(daemon.attention_of(PaneId(7)), Some(AttentionState::NeedsInput));
        // Notifier called.
        assert_eq!(notifier.calls(), vec![(PaneId(7), AttentionState::NeedsInput)]);
    }

    #[tokio::test]
    async fn active_hook_clears_attention_back_to_working() {
        let notifier = Arc::new(FakeNotifier::new());
        let daemon = Arc::new(Daemon::new_with(
            notifier.clone(),
            PathBuf::from("/tmp/unused.sock"),
        ));

        // Deliver each hook over its own connection, awaiting the handler so the
        // transitions apply in order (handle_hook_conn applies the event then returns).
        let deliver = |kind: HookKind| {
            let d = daemon.clone();
            async move {
                let (client_io, server_io) = tokio::io::duplex(4096);
                let handler = tokio::spawn(async move { d.handle_hook_conn(server_io).await.unwrap() });
                MsgStream::new(client_io)
                    .send(&HookEvent { agent_id: PaneId(9), kind })
                    .await
                    .unwrap();
                handler.await.unwrap();
            }
        };

        // First a Notification puts the agent in NeedsInput...
        deliver(HookKind::Notification).await;
        assert_eq!(daemon.attention_of(PaneId(9)), Some(AttentionState::NeedsInput));
        // ...then an Active hook flips it back to Working.
        deliver(HookKind::Active).await;
        assert_eq!(daemon.attention_of(PaneId(9)), Some(AttentionState::Working));
    }
}
