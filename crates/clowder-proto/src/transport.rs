// SPDX-License-Identifier: Apache-2.0

use crate::{ClientToDaemon, DaemonToClient, PaneId};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub trait Transport: Send {}

pub struct MsgStream<S> {
    framed: Framed<S, LengthDelimitedCodec>,
}

impl<S> MsgStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub fn new(io: S) -> Self {
        Self { framed: Framed::new(io, LengthDelimitedCodec::new()) }
    }

    pub async fn send<M: Serialize>(&mut self, msg: &M) -> Result<()> {
        let bytes = postcard::to_stdvec(msg)?;
        self.framed.send(bytes.into()).await?;
        Ok(())
    }

    pub async fn recv<M: DeserializeOwned>(&mut self) -> Result<Option<M>> {
        match self.framed.next().await {
            Some(frame) => {
                let frame = frame?;
                let msg = postcard::from_bytes(&frame)?;
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_roundtrip_over_duplex() {
        // in-memory bidirectional pipe standing in for a UnixStream
        let (a, b) = tokio::io::duplex(4096);
        let mut client = MsgStream::new(a);
        let mut server = MsgStream::new(b);

        let sent = ClientToDaemon::Attach { pane: PaneId(1) };
        client.send(&sent).await.unwrap();
        let got: ClientToDaemon = server.recv().await.unwrap().unwrap();
        assert_eq!(got, sent);

        let reply = DaemonToClient::Attached { pane: PaneId(1), cols: 80, rows: 24 };
        server.send(&reply).await.unwrap();
        let got: DaemonToClient = client.recv().await.unwrap().unwrap();
        assert_eq!(got, reply);
    }

    #[tokio::test]
    async fn recv_returns_none_on_eof() {
        let (a, b) = tokio::io::duplex(64);
        let client = MsgStream::new(a);
        drop(client); // close one end
        let mut server: MsgStream<_> = MsgStream::new(b);
        let got: Option<ClientToDaemon> = server.recv().await.unwrap();
        assert!(got.is_none());
    }
}
