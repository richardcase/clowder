use anyhow::Result;
use muxy_daemon::server::Daemon;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> Result<()> {
    let sock_path = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());
    let daemon = Arc::new(Daemon::new());
    let hook_path = daemon.hook_sock().to_path_buf();

    let control_path =
        std::env::var("MUXY_CONTROL_SOCK").unwrap_or_else(|_| "/tmp/muxy-control.sock".into());

    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&hook_path);
    let _ = std::fs::remove_file(&control_path);
    let client_listener = UnixListener::bind(&sock_path)?;
    let hook_listener = UnixListener::bind(&hook_path)?;
    let control_listener = UnixListener::bind(&control_path)?;
    eprintln!(
        "muxy-daemon: client={sock_path} hook={} control={control_path}",
        hook_path.display()
    );

    let hooks = daemon.clone();
    tokio::spawn(async move { let _ = hooks.serve_hooks(hook_listener).await; });

    let control = daemon.clone();
    tokio::spawn(async move { let _ = control.serve_control_json(control_listener).await; });

    daemon.serve(client_listener).await
}
