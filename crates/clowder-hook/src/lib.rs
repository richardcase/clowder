use anyhow::Result;
use clowder_proto::{HookEvent, MsgStream};
use std::path::Path;
use tokio::net::UnixStream;

/// Connect to the daemon's hook socket and send exactly one HookEvent.
pub async fn send_hook(sock: &Path, event: HookEvent) -> Result<()> {
    let stream = UnixStream::connect(sock).await?;
    let mut msgs = MsgStream::new(stream);
    msgs.send(&event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clowder_proto::{HookKind, PaneId};

    #[tokio::test]
    async fn send_hook_delivers_one_event() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("hook.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();

        let event = HookEvent { agent_id: PaneId(42), kind: HookKind::Stop };
        let event2 = event.clone();
        let sock2 = sock.clone();
        let client = tokio::spawn(async move { send_hook(&sock2, event2).await.unwrap() });

        let (stream, _) = listener.accept().await.unwrap();
        let mut msgs = MsgStream::new(stream);
        let got: HookEvent = msgs.recv().await.unwrap().unwrap();
        assert_eq!(got, event);
        client.await.unwrap();
    }
}
