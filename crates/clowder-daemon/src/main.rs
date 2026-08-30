// SPDX-License-Identifier: Apache-2.0

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
    let hook_path = config.hook_sock.clone();
    let remote_listen = config.remote_listen.clone();
    let config_remote_tls = config.remote_tls;
    // The daemon itself is built AFTER the sockets are bound — see the login-env capture below.

    // Sockets may live in a per-user dir that doesn't exist yet; create each parent. A failure here
    // is not fatal by itself (the dir may already exist and be fine) but it IS the cause of the
    // otherwise-opaque bind error below, so say so rather than discarding it.
    for p in [&sock_path, &hook_path, &control_path] {
        if let Some(dir) = p.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!("could not create socket dir {}: {e}", dir.display());
            }
        }
    }

    // Single-instance guard: refuse to start if another daemon already holds the lock.
    let lock_path = InstanceLock::default_path();
    let lock = match InstanceLock::acquire(&lock_path) {
        Ok(Some(l)) => l,
        // ONLY a genuine second instance yields. Exit 3 is the distinct code that tells the
        // supervising app "another daemon already owns this" — and the app treats it as permanent,
        // never relaunching. Anything else must NOT land here: a mkdir/permissions failure that
        // exited 3 left the app with no daemon, silently, for ever.
        Ok(None) => {
            tracing::error!("another clowder-daemon is already running (lock held at {})", lock_path.display());
            std::process::exit(3);
        }
        // A real failure: propagate so `main` exits 1, which the supervisor retries with backoff.
        Err(e) => return Err(e.context("could not acquire the single-instance lock")),
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

    // Only NOW work out what environment panes get. This runs the user's login shell, which can
    // take the better part of a second (or hang on a bad rc file), and it must not delay the binds
    // above: a client that connects meanwhile waits in the accept backlog instead of getting
    // ECONNREFUSED and entering the app's reconnect ramp.
    //
    // It must, however, finish before `reconcile()` — that respawns the whole fleet, and a
    // respawned agent with the wrong PATH is exactly the bug being fixed.
    let pane_env = resolve_pane_env(&config).await;
    let daemon = Arc::new(Daemon::new_from_config(config).with_pane_env(pane_env));

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
            if config_remote_tls {
                tracing::info!(%addr, "remote listener bound to a non-loopback/non-tailnet address, protected by TLS + token auth");
            } else {
                tracing::warn!(%addr, "remote listener bound to a non-loopback/non-tailnet address — plaintext with NO authentication; set [remote] tls = true, or expose only behind a trusted tunnel (SSH -L / Tailscale)");
            }
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

/// The environment every PTY child will start from.
///
/// A GUI-launched app's daemon inherits launchd's `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, in which
/// `claude` does not exist — so it asks the user's login shell what the environment should be. A
/// failure here is never fatal: panes fall back to inheriting the daemon's own environment, which
/// is exactly the pre-#76 behaviour.
async fn resolve_pane_env(config: &clowder_config::Config) -> clowder_daemon::PaneEnv {
    use clowder_daemon::login_env;

    let fallback = || login_env::PaneEnv::inherited(&config.shell);
    if !config.capture_login_env {
        tracing::info!("login-env capture disabled; panes inherit the daemon's environment");
        return fallback();
    }

    let spec = login_env::CaptureSpec {
        shell: config.shell.clone(),
        timeout: std::time::Duration::from_millis(config.login_env_timeout_ms),
        cwd: std::env::var_os("HOME").map(std::path::PathBuf::from),
    };
    let started = std::time::Instant::now();
    match login_env::capture(&spec).await {
        Ok(captured) => {
            let env = login_env::PaneEnv::resolve(
                Some(captured),
                login_env::env_snapshot(),
                login_env::exe_dir().as_deref(),
                &config.shell,
            );
            // The one line that answers "why can't it find claude?" from daemon.log.
            tracing::info!(
                shell = %config.shell,
                vars = env.len(),
                took_ms = started.elapsed().as_millis() as u64,
                "login-env captured"
            );
            tracing::debug!(path = env.get("PATH").unwrap_or(""), "login-env PATH");
            env
        }
        Err(e) => {
            tracing::warn!(
                shell = %config.shell,
                "login-env capture failed ({e:#}); panes inherit the daemon's environment, so an \
                 agent binary that isn't on it will not be found (see issue #76). Set \
                 CLOWDER_CAPTURE_LOGIN_ENV=0 to silence this."
            );
            fallback()
        }
    }
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
