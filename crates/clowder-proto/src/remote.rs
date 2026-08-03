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

/// Write the channel hello (channel byte + length-prefixed optional token) that prefixes a
/// remote connection. The token is present only on the TLS path; plaintext sends `None`.
pub async fn write_hello<W: AsyncWrite + Unpin>(
    w: &mut W,
    channel: Channel,
    token: Option<&str>,
) -> Result<()> {
    w.write_u8(channel.to_byte()).await?;
    let bytes = token.map(str::as_bytes).unwrap_or(&[]);
    if bytes.len() > u16::MAX as usize {
        bail!("hello token too long");
    }
    w.write_u16(bytes.len() as u16).await?;
    w.write_all(bytes).await?;
    w.flush().await?;
    Ok(())
}

/// Read the channel hello + optional token. Bounds the token length so a hostile peer cannot
/// force a large allocation.
pub async fn read_hello<R: AsyncRead + Unpin>(r: &mut R) -> Result<(Channel, Option<String>)> {
    let channel = Channel::from_byte(r.read_u8().await?)?;
    let len = r.read_u16().await? as usize;
    if len > 4096 {
        bail!("hello token length {len} exceeds limit");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let token = if len == 0 { None } else { Some(String::from_utf8(buf)?) };
    Ok((channel, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello_roundtrips_channel_and_token() {
        for (ch, tok) in [
            (Channel::Control, None),
            (Channel::Render, Some("s3cr3t-token".to_string())),
        ] {
            let (mut a, mut b) = tokio::io::duplex(64);
            write_hello(&mut a, ch, tok.as_deref()).await.unwrap();
            let (rch, rtok) = read_hello(&mut b).await.unwrap();
            assert_eq!(rch, ch);
            assert_eq!(rtok, tok);
        }
    }

    #[tokio::test]
    async fn unknown_channel_byte_errors() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u8(9).await.unwrap();
        assert!(read_hello(&mut b).await.is_err());
    }
}
