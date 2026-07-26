use anyhow::Result;
use muxy_client::pump;
use muxy_proto::PaneId;
use tokio::net::UnixStream;

#[tokio::main]
async fn main() -> Result<()> {
    let sock = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());
    let pane = PaneId(
        std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1),
    );

    let stream = UnixStream::connect(&sock).await?;

    // Put the real terminal in raw mode so keystrokes reach the pane unbuffered.
    crossterm::terminal::enable_raw_mode()?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let result = pump(stream, pane, stdin, stdout).await;
    crossterm::terminal::disable_raw_mode()?;
    result
}
