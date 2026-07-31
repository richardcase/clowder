use anyhow::{bail, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Which channel a remote (TCP) connection carries. Sent as a single byte at the
/// very start of the connection, before any channel-specific framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Control,
    Render,
}

impl Channel {
    fn to_byte(self) -> u8 {
        match self {
            Channel::Control => 1,
            Channel::Render => 2,
        }
    }
    fn from_byte(b: u8) -> Result<Channel> {
        match b {
            1 => Ok(Channel::Control),
            2 => Ok(Channel::Render),
            other => bail!("unknown channel hello byte {other}"),
        }
    }
}

/// Write the one-byte channel hello that prefixes a remote connection.
pub async fn write_hello<W: AsyncWrite + Unpin>(w: &mut W, channel: Channel) -> Result<()> {
    w.write_u8(channel.to_byte()).await?;
    w.flush().await?;
    Ok(())
}

/// Read the one-byte channel hello from the start of a remote connection.
/// `read_u8` reads exactly one byte (no over-read), so the remaining stream stays
/// correctly framed for the channel body.
pub async fn read_hello<R: AsyncRead + Unpin>(r: &mut R) -> Result<Channel> {
    let b = r.read_u8().await?;
    Channel::from_byte(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello_roundtrips_both_channels() {
        for ch in [Channel::Control, Channel::Render] {
            let (mut a, mut b) = tokio::io::duplex(64);
            write_hello(&mut a, ch).await.unwrap();
            assert_eq!(read_hello(&mut b).await.unwrap(), ch);
        }
    }

    #[tokio::test]
    async fn unknown_channel_byte_errors() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u8(9).await.unwrap();
        assert!(read_hello(&mut b).await.is_err());
    }
}
