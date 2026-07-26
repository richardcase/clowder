use anyhow::Result;
use muxy_daemon::server::Daemon;
use muxy_daemon::PaneCommand;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> Result<()> {
    let sock_path = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;

    let daemon = Arc::new(Daemon::new());
    // M0a: launch a single login shell pane so a client has something to attach to.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let pane = daemon.spawn_pane(
        PaneCommand { program: shell, args: vec![], cwd: None, env: vec![] },
        80,
        24,
    )?;
    eprintln!("muxy-daemon listening on {sock_path}, pane {pane:?}");

    daemon.serve(listener).await
}
