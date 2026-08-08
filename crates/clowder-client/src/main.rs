use anyhow::{anyhow, Result};
use clowder_client::{
    add_project_via_control, attach, list_projects_via_control, remove_project_via_control,
    spawn_via_control,
};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("spawn") => {
            let project = args.get(2).ok_or_else(|| anyhow!("usage: clowder spawn <project> <name> [adapter]"))?;
            let task = args.get(3).ok_or_else(|| anyhow!("usage: clowder spawn <project> <name> [adapter]"))?;
            let adapter = args.get(4).map(|s| s.as_str()).unwrap_or("claude");
            let sock = clowder_config::Config::load().control_sock;
            let pane = spawn_via_control(&sock, project, task, adapter).await?;
            println!("{}", pane.0);
            Ok(())
        }
        Some("project") => {
            let sock = clowder_config::Config::load().control_sock;
            match args.get(2).map(|s| s.as_str()) {
                Some("add") => {
                    let path = args.get(3).ok_or_else(|| anyhow!("usage: clowder project add <path>"))?;
                    let p = add_project_via_control(&sock, path).await?;
                    println!("{} ({})", p.path, p.kind);
                    Ok(())
                }
                Some("list") => {
                    for p in list_projects_via_control(&sock).await? {
                        println!("{}\t{}\t{}", p.kind, p.name, p.path);
                    }
                    Ok(())
                }
                Some("rm") => {
                    let path = args.get(3).ok_or_else(|| anyhow!("usage: clowder project rm <path>"))?;
                    remove_project_via_control(&sock, path).await?;
                    Ok(())
                }
                _ => Err(anyhow!("usage: clowder project <add|list|rm> [path]")),
            }
        }
        Some("attach") => {
            let pane = args.get(2).and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("usage: clowder attach <pane-id>"))?;
            attach(pane).await
        }
        Some("connect") => {
            let cfg = clowder_config::Config::load();
            let flags = clowder_client::remote_cli::parse_flags(&args[2..]).map_err(anyhow::Error::msg)?;
            flags.reject_unknown(&["socket-dir"]).map_err(anyhow::Error::msg)?;
            let hosts = clowder_config::hosts::merged_hosts(
                clowder_config::hosts::HostsStore::default_store().load(),
                &cfg,
            );
            let target = clowder_client::target::resolve_target(flags.positional(0), &hosts, &cfg)
                .map_err(anyhow::Error::msg)?;

            // The caller owns the forwarder's socket path. --socket-dir is used verbatim; the
            // default is deliberately FLAT (`<control parent>/remote`, no per-host segment),
            // because it is a compatibility guarantee: the macOS app derives this exact path in
            // ClowderCore's `forwarderSocketDir`, and shell users have it in their env already.
            // A caller that wants per-host isolation asks for it with --socket-dir rather than
            // having the layout changed underneath it.
            let dir = match flags.str("socket-dir") {
                Some(d) => std::path::PathBuf::from(d),
                None => cfg
                    .control_sock
                    .parent()
                    .ok_or_else(|| anyhow!("cannot derive forwarder socket dir"))?
                    .join("remote"),
            };

            // Fail fast when the very first dial never lands. Without this the forwarder binds
            // its sockets and lives on, and the app's supervisor relaunches it forever behind a
            // permanent "Reconnecting…" with no way to tell a typo from a daemon that is down.
            // Exit 4 is the signal to stop and show the user (see DaemonSupervisor in M11b).
            const FIRST_DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
            if tokio::time::timeout(
                FIRST_DIAL_TIMEOUT,
                tokio::net::TcpStream::connect(&target.address),
            )
            .await
            .map_err(|_| ())
            .and_then(|r| r.map_err(|_| ()))
            .is_err()
            {
                eprintln!(
                    "clowder connect: cannot reach {} at {} — check the address, and that the daemon \
                     is running with [remote] listen set",
                    target.label, target.address
                );
                std::process::exit(4);
            }

            clowder_client::forward::forward(target, dir).await
        }
        Some("remote-host") => {
            // Print the resolved [remote] host (or an empty line) so the macOS app can decide
            // local-vs-remote mode without parsing config.toml itself.
            println!("{}", clowder_config::Config::load().remote_host.unwrap_or_default());
            Ok(())
        }
        Some("remote-token") => {
            let tok_p = clowder_config::remote_token_path();
            let cert_p = clowder_config::remote_cert_path();
            let token = std::fs::read_to_string(&tok_p)
                .map_err(|e| anyhow!("no remote token at {} ({e}); start the daemon with [remote] tls=true first", tok_p.display()))?;
            let cert_pem = std::fs::read_to_string(&cert_p)?;
            let mut rd = std::io::BufReader::new(cert_pem.as_bytes());
            let mut certs = rustls_pemfile::certs(&mut rd);
            let der = certs.next().ok_or_else(|| anyhow!("no cert"))??.to_vec();
            println!("token:       {}", token.trim());
            println!("fingerprint: {}", clowder_proto::cert_fingerprint_hex(&der));
            Ok(())
        }
        Some("remote") => clowder_client::remote_cli::run(&args[2..]).await,
        // Legacy: `clowder <pane-id>` still attaches.
        Some(other) if other.parse::<u64>().is_ok() => attach(other.parse().unwrap()).await,
        _ => Err(anyhow!("usage: clowder <spawn|project|attach|connect|remote|remote-host|remote-token> ...")),
    }
}
