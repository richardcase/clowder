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

/// Resolve the `muxy-hook` binary the injected hooks should invoke. The agent process
/// (e.g. `claude`) runs these hooks and does not necessarily have `muxy-hook` on its PATH
/// — a dev running the daemon from `cargo`/`target/debug` certainly won't — so prefer an
/// absolute path. Order: `$MUXY_HOOK_BIN`, then a sibling of the running daemon executable
/// (`target/debug/muxy-hook` next to `muxy-daemon`), then a bare `muxy-hook` (assume PATH).
pub(crate) fn muxy_hook_bin() -> String {
    if let Ok(p) = std::env::var("MUXY_HOOK_BIN") {
        if !p.is_empty() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|d| d.join("muxy-hook")) {
            if sibling.exists() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    "muxy-hook".to_string()
}

/// Real Claude Code adapter: writes .claude/settings.local.json whose Notification/Stop
/// hooks invoke `muxy-hook`, plus a .claude/.gitignore so that hook config isn't committed
/// into the agent's own branch.
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn provision_hooks(&self, worktree: &Path, _agent_id: PaneId, _hook_sock: &Path) -> Result<()> {
        let dir = worktree.join(".claude");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(".gitignore"), "settings.local.json\n")?;
        // Single-quote the resolved path so a binary path containing spaces still runs when
        // the tool executes the hook command through a shell.
        let bin = muxy_hook_bin();
        let hook = |event: &str| {
            serde_json::json!([{ "hooks": [{ "type": "command", "command": format!("'{bin}' --event {event}") }] }])
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
        // Notification + Stop hooks both invoke muxy-hook with the right event. The binary
        // is now a resolved (usually absolute) path, so assert on content/suffix, not an
        // exact bare string.
        let notif = v["hooks"]["Notification"][0]["hooks"][0]["command"].as_str().unwrap();
        let stop = v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(notif.contains("muxy-hook") && notif.ends_with("--event notification"), "got: {notif}");
        assert!(stop.contains("muxy-hook") && stop.ends_with("--event stop"), "got: {stop}");

        // The hook settings themselves must be git-ignored so they don't get committed
        // into the agent's own branch.
        let gitignore = std::fs::read_to_string(dir.path().join(".claude/.gitignore")).unwrap();
        assert!(
            gitignore.lines().any(|l| l.trim() == "settings.local.json"),
            "expected .claude/.gitignore to contain settings.local.json, got: {gitignore:?}"
        );
    }
}
