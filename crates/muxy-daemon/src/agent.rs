use crate::PaneCommand;
use anyhow::Result;
use muxy_proto::PaneId;
use std::path::Path;

pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    /// Write the tool's hook config into the fresh worktree so its hooks call `muxy-hook`.
    fn provision_hooks(&self, worktree: &Path, agent_id: PaneId, hook_sock: &Path) -> Result<()>;
    /// The command to launch the agent (cwd/env are filled in by the daemon).
    fn launch_command(&self, worktree: &Path) -> PaneCommand;
}

/// Real Claude Code adapter: writes a git-ignored .claude/settings.local.json whose
/// Notification/Stop hooks invoke `muxy-hook`.
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn provision_hooks(&self, worktree: &Path, _agent_id: PaneId, _hook_sock: &Path) -> Result<()> {
        let dir = worktree.join(".claude");
        std::fs::create_dir_all(&dir)?;
        let hook = |event: &str| {
            serde_json::json!([{ "hooks": [{ "type": "command", "command": format!("muxy-hook --event {event}") }] }])
        };
        let settings = serde_json::json!({
            "hooks": { "Notification": hook("notification"), "Stop": hook("stop") }
        });
        std::fs::write(dir.join("settings.local.json"), serde_json::to_vec_pretty(&settings)?)?;
        Ok(())
    }

    fn launch_command(&self, _worktree: &Path) -> PaneCommand {
        PaneCommand { program: "claude".into(), args: vec![], cwd: None, env: vec![] }
    }
}

/// Test adapter: runs a caller-supplied benign command in the worktree and drops a marker
/// file instead of real hooks. No live agent, no network.
pub struct SyntheticAdapter {
    pub command: PaneCommand,
}

impl AgentAdapter for SyntheticAdapter {
    fn id(&self) -> &'static str {
        "synthetic"
    }

    fn provision_hooks(&self, worktree: &Path, agent_id: PaneId, _hook_sock: &Path) -> Result<()> {
        std::fs::write(worktree.join(".muxy-agent"), agent_id.0.to_string())?;
        Ok(())
    }

    fn launch_command(&self, _worktree: &Path) -> PaneCommand {
        self.command.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_adapter_writes_hook_settings() {
        let dir = tempfile::tempdir().unwrap();
        ClaudeAdapter
            .provision_hooks(dir.path(), PaneId(1), Path::new("/tmp/h.sock"))
            .unwrap();
        let raw = std::fs::read_to_string(dir.path().join(".claude/settings.local.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // Notification + Stop hooks both call muxy-hook with the right event.
        let notif = v["hooks"]["Notification"][0]["hooks"][0]["command"].as_str().unwrap();
        let stop = v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(notif, "muxy-hook --event notification");
        assert_eq!(stop, "muxy-hook --event stop");
    }
}
