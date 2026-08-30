// SPDX-License-Identifier: Apache-2.0

use anyhow::{anyhow, Result};
use clowder_hook::send_hook;
use clowder_proto::{HookEvent, HookKind, PaneId};
use std::io::Read;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    // Usage: clowder-hook --event <notification|stop>
    let mut kind = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--event" {
            kind = match args.next().as_deref() {
                Some("notification") => Some(HookKind::Notification),
                Some("stop") => Some(HookKind::Stop),
                Some("active") => Some(HookKind::Active),
                other => return Err(anyhow!("unknown --event value: {other:?}")),
            };
        }
    }
    let kind = kind.ok_or_else(|| anyhow!("--event <notification|stop> is required"))?;

    // The tool pipes its hook JSON on stdin; M0b does not need it — drain and discard.
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);

    let agent_id: u64 = std::env::var("CLOWDER_AGENT_ID")
        .map_err(|_| anyhow!("CLOWDER_AGENT_ID not set"))?
        .parse()
        .map_err(|_| anyhow!("CLOWDER_AGENT_ID not a u64"))?;
    let sock = PathBuf::from(std::env::var("CLOWDER_HOOK_SOCK").map_err(|_| anyhow!("CLOWDER_HOOK_SOCK not set"))?);

    send_hook(&sock, HookEvent { agent_id: PaneId(agent_id), kind }).await
}
