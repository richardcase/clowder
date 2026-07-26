use anyhow::Result;
use muxy_daemon::server::Daemon;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> Result<()> {
    let sock_path = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());
    let daemon = Arc::new(Daemon::new());
    let hook_path = daemon.hook_sock().to_path_buf();

    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&hook_path);
    let client_listener = UnixListener::bind(&sock_path)?;
    let hook_listener = UnixListener::bind(&hook_path)?;
    eprintln!("muxy-daemon: client={sock_path} hook={}", hook_path.display());

    let hooks = daemon.clone();
    tokio::spawn(async move { let _ = hooks.serve_hooks(hook_listener).await; });

    daemon.serve(client_listener).await
}
