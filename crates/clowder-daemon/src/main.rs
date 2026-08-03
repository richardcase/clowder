use anyhow::Result;
use clowder_daemon::instance::{remove_files, InstanceLock};
use clowder_daemon::server::Daemon;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> Result<()> {
    clowder_daemon::logging::init();
    let config = clowder_config::Config::load();
    let sock_path = config.client_sock.clone();
    let control_path = config.control_sock.clone();
    let remote_listen = config.remote_listen.clone();
    let config_remote_tls = config.remote_tls;
    let daemon = Arc::new(Daemon::new_from_config(config));
    let hook_path = daemon.hook_sock().to_path_buf();

    // Sockets may live in a per-user dir that doesn't exist yet; create each parent.
    for p in [&sock_path, &hook_path, &control_path] {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
    }

    // Single-instance guard: refuse to start if another daemon already holds the lock.
    let lock_path = InstanceLock::default_path();
    let lock = match InstanceLock::acquire(&lock_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("{e}");
            // Distinct code so a supervising parent can tell "another instance already owns the daemon"
            // (yield) apart from a generic startup error / `main` Err (which exits 1 → relaunch).
            std::process::exit(3);
        }
    };

    // We own the instance: clear any stale sockets, then bind.
    remove_files(&[&sock_path, &hook_path, &control_path]);
    let client_listener = UnixListener::bind(&sock_path)?;
    let hook_listener = UnixListener::bind(&hook_path)?;
    let control_listener = UnixListener::bind(&control_path)?;
    tracing::info!(
        client = %sock_path.display(),
        hook = %hook_path.display(),
        control = %control_path.display(),
        pid_lock = %lock.path().display(),
        "clowder-daemon listening"
    );

    // Agents don't survive their daemon's PTYs dying with it; re-spawn every agent still
    // recorded in the durable registry (pruning any whose worktree/adapter is gone) before
    // serving clients, so a restarted daemon comes back up with its fleet intact.
    tracing::info!("reconciling agent registry");
    daemon.reconcile();

    // Coalesced layout persistence: ratio drags mark the agent dirty; this task flushes them.
    let _layout_flusher = daemon.spawn_layout_flusher();

    if let Some(addr_str) = remote_listen {
        let addr: std::net::SocketAddr = addr_str
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid [remote] listen address {addr_str:?}: {e}"))?;
        let tcp = tokio::net::TcpListener::bind(addr).await?;
        if clowder_daemon::remote::should_warn_exposed(&addr) {
            tracing::warn!(%addr, "remote listener bound to a non-loopback/non-tailnet address — Phase A has NO authentication; expose only behind a trusted tunnel (SSH -L / Tailscale)");
        }
        // Fail closed: if TLS is enabled but credential setup fails, refuse to start the
        // remote listener rather than silently falling back to plaintext.
        let tls = if config_remote_tls {
            let creds = clowder_daemon::remote_tls::load_or_generate()
                .map_err(|e| anyhow::anyhow!("[remote] tls enabled but credential setup failed: {e}"))?;
            tracing::info!(
                "remote TLS enabled — token: {}  cert fingerprint (sha256): {}",
                creds.token, clowder_daemon::remote_tls::fingerprint(&creds)
            );
            Some(clowder_daemon::remote::build_remote_tls(&creds)?)
        } else {
            None
        };
        tracing::info!(%addr, "clowder-daemon remote TCP listener enabled");
        let remote = daemon.clone();
        tokio::spawn(async move {
            if let Some(line) = clowder_daemon::logging::conn_error_line("remote server", remote.serve_remote(tcp, tls).await) {
                tracing::error!("{line}");
            }
        });
    }

    let hooks = daemon.clone();
    tokio::spawn(async move {
        if let Some(line) = clowder_daemon::logging::conn_error_line("hook server", hooks.serve_hooks(hook_listener).await) {
            tracing::error!("{line}");
        }
    });

    let control = daemon.clone();
    tokio::spawn(async move {
        if let Some(line) = clowder_daemon::logging::conn_error_line("control server", control.serve_control_json(control_listener).await) {
            tracing::error!("{line}");
        }
    });

    // Serve until a shutdown signal arrives, then kill children and clean up.
    let serving = daemon.clone();
    let result = tokio::select! {
        r = serving.serve(client_listener) => r,
        _ = shutdown_signal() => {
            tracing::info!("received shutdown signal, stopping");
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
