use anyhow::{anyhow, Result};
use clowder_client::{attach, spawn_via_control};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("spawn") => {
            let project = args.get(2).ok_or_else(|| anyhow!("usage: clowder spawn <project> <task> [adapter]"))?;
            let task = args.get(3).ok_or_else(|| anyhow!("usage: clowder spawn <project> <task> [adapter]"))?;
            let adapter = args.get(4).map(|s| s.as_str()).unwrap_or("claude");
            let sock = clowder_config::Config::load().control_sock;
            let pane = spawn_via_control(&sock, project, task, adapter).await?;
            println!("{}", pane.0);
            Ok(())
        }
        Some("attach") => {
            let pane = args.get(2).and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("usage: clowder attach <pane-id>"))?;
            attach(pane).await
        }
        Some("connect") => {
            let cfg = clowder_config::Config::load();
            let host = args.get(2).cloned()
                .or(cfg.remote_host.clone())
                .ok_or_else(|| anyhow!("usage: clowder connect <host:port>  (or set [remote] host / CLOWDER_REMOTE_HOST)"))?;
            let dir = cfg.control_sock.parent()
                .ok_or_else(|| anyhow!("cannot derive forwarder socket dir"))?
                .join("remote");
            clowder_client::forward::forward(host, dir).await
        }
        Some("remote-host") => {
            // Print the resolved [remote] host (or an empty line) so the macOS app can decide
            // local-vs-remote mode without parsing config.toml itself.
            println!("{}", clowder_config::Config::load().remote_host.unwrap_or_default());
            Ok(())
        }
        // Legacy: `clowder <pane-id>` still attaches.
        Some(other) if other.parse::<u64>().is_ok() => attach(other.parse().unwrap()).await,
        _ => Err(anyhow!("usage: clowder <spawn|attach|connect|remote-host> ...")),
    }
}
