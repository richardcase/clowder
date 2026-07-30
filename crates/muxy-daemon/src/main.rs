use anyhow::Result;
use muxy_daemon::instance::{remove_files, InstanceLock};
use muxy_daemon::server::Daemon;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> Result<()> {
    let config = muxy_config::Config::load();
    let sock_path = config.client_sock.clone();
    let control_path = config.control_sock.clone();
    let daemon = Arc::new(Daemon::new_from_config(config));
    let hook_path = daemon.hook_sock().to_path_buf();

    // Single-instance guard: refuse to start if another daemon already holds the lock.
    let lock_path = InstanceLock::default_path();
    let lock = match InstanceLock::acquire(&lock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("muxy-daemon: {e}");
            std::process::exit(1);
        }
    };

    // We own the instance: clear any stale sockets, then bind.
    remove_files(&[&sock_path, &hook_path, &control_path]);
    let client_listener = UnixListener::bind(&sock_path)?;
    let hook_listener = UnixListener::bind(&hook_path)?;
    let control_listener = UnixListener::bind(&control_path)?;
    eprintln!(
        "muxy-daemon: client={} hook={} control={} pid_lock={}",
        sock_path.display(),
        hook_path.display(),
        control_path.display(),
        lock.path().display()
    );

    let hooks = daemon.clone();
    tokio::spawn(async move { let _ = hooks.serve_hooks(hook_listener).await; });

    let control = daemon.clone();
    tokio::spawn(async move { let _ = control.serve_control_json(control_listener).await; });

    // Serve until a shutdown signal arrives, then kill children and clean up.
    let serving = daemon.clone();
    let result = tokio::select! {
        r = serving.serve(client_listener) => r,
        _ = shutdown_signal() => {
            eprintln!("muxy-daemon: received shutdown signal, stopping");
            Ok(())
        }
    };

    daemon.shutdown();
    remove_files(&[&sock_path, &hook_path, &control_path, &lock_path]);
    drop(lock); // release the advisory flock
    result
}

/// Resolve when the daemon receives SIGTERM or SIGINT.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}
