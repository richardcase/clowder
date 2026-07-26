use anyhow::Result;
use muxy_client::pump;
use muxy_proto::PaneId;
use tokio::net::UnixStream;

/// RAII guard that restores the terminal from raw mode when dropped, even on
/// error paths or panics/unwinds — so a crash in `pump` never leaves the
/// user's terminal wrecked.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(RawModeGuard)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

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
    let _guard = RawModeGuard::enable()?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    pump(stream, pane, stdin, stdout).await
    // _guard drops here (on any exit path, including unwind), restoring raw mode;
    // pump's Result is returned directly, unmasked.
}
