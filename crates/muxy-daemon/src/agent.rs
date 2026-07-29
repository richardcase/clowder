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
    /// Whether this adapter injects tool-native attention hooks. If false, the daemon runs the
    /// VT-signal fallback scanner for the agent instead.
    fn provides_hooks(&self) -> bool;
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
        // Notification → NeedsInput, Stop → Completed, and UserPromptSubmit/PreToolUse →
        // Active (Working) so the attention indicator clears back to green once the agent
        // resumes work after the user deals with a prompt (or approves a tool).
        let settings = serde_json::json!({
            "hooks": {
                "Notification": hook("notification"),
                "Stop": hook("stop"),
                "UserPromptSubmit": hook("active"),
                "PreToolUse": hook("active"),
            }
        });
        std::fs::write(dir.join("settings.local.json"), serde_json::to_vec_pretty(&settings)?)?;
        Ok(())
    }

    fn launch_command(&self, _worktree: &Path) -> PaneCommand {
        PaneCommand { program: "claude".into(), args: vec![], cwd: None, env: vec![] }
    }

    fn provides_hooks(&self) -> bool {
        true
    }
}

/// OpenAI Codex adapter. Codex's legacy `notify` fires only on `agent-turn-complete`,
/// invoking an arbitrary program with a JSON string as the trailing argv arg. A project
/// `.codex/config.toml` cannot set `notify` (a machine-local key), so we wire it at launch
/// via `-c`. muxy-hook self-IDs from the MUXY_AGENT_ID/MUXY_HOOK_SOCK env the daemon injects
/// and ignores the trailing JSON, so turn-complete → `--event stop` → Completed.
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn provision_hooks(&self, _worktree: &Path, _agent_id: PaneId, _hook_sock: &Path) -> Result<()> {
        // No file to write: the notify hook is a launch argument, not provisioned config.
        Ok(())
    }

    fn launch_command(&self, _worktree: &Path) -> PaneCommand {
        let bin = muxy_hook_bin();
        // TOML array-of-argv value for the `-c notify=` override. Quote the resolved
        // muxy-hook path so a path containing spaces still parses.
        let notify = format!("notify=[\"{bin}\",\"--event\",\"stop\"]");
        PaneCommand { program: "codex".into(), args: vec!["-c".into(), notify], cwd: None, env: vec![] }
    }

    fn provides_hooks(&self) -> bool {
        true
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

    fn provides_hooks(&self) -> bool {
        false
    }
}

/// A spawnable adapter's stable id + human label (single source of truth for spawn + M4b discovery).
pub struct AdapterDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
}

/// The adapters a client may spawn.
pub fn adapter_descriptors() -> &'static [AdapterDescriptor] {
    &[
        AdapterDescriptor { id: "claude", display_name: "Claude Code" },
        AdapterDescriptor { id: "codex", display_name: "OpenAI Codex" },
        AdapterDescriptor { id: "shell", display_name: "Shell" },
    ]
}

/// Construct an adapter by id, or `None` for an unknown id.
pub fn build_adapter(id: &str) -> Option<Box<dyn AgentAdapter>> {
    match id {
        "claude" => Some(Box::new(ClaudeAdapter)),
        "codex" => Some(Box::new(CodexAdapter)),
        "shell" => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            Some(Box::new(SyntheticAdapter {
                command: PaneCommand { program: shell, args: vec![], cwd: None, env: vec![] },
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapters_declare_hook_support() {
        assert!(ClaudeAdapter.provides_hooks(), "claude has hooks");
        let synthetic = SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec![], cwd: None, env: vec![] },
        };
        assert!(!synthetic.provides_hooks(), "shell/synthetic has no hooks");
    }

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

        // UserPromptSubmit + PreToolUse both emit `active` so the indicator returns to Working.
        let prompt = v["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"].as_str().unwrap();
        let pretool = v["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(prompt.ends_with("--event active"), "got: {prompt}");
        assert!(pretool.ends_with("--event active"), "got: {pretool}");

        // The hook settings themselves must be git-ignored so they don't get committed
        // into the agent's own branch.
        let gitignore = std::fs::read_to_string(dir.path().join(".claude/.gitignore")).unwrap();
        assert!(
            gitignore.lines().any(|l| l.trim() == "settings.local.json"),
            "expected .claude/.gitignore to contain settings.local.json, got: {gitignore:?}"
        );
    }

    #[test]
    fn codex_launch_command_wires_notify_to_muxy_hook() {
        let cmd = CodexAdapter.launch_command(std::path::Path::new("/tmp/ws"));
        assert_eq!(cmd.program, "codex");
        let bin = crate::agent::muxy_hook_bin();
        // Codex fires notify only on agent-turn-complete → --event stop → Completed.
        assert_eq!(cmd.args, vec!["-c".to_string(), format!("notify=[\"{bin}\",\"--event\",\"stop\"]")]);
    }

    #[test]
    fn codex_provides_hooks_and_provision_writes_nothing() {
        assert!(CodexAdapter.provides_hooks(), "codex has a native notify hook");
        assert_eq!(CodexAdapter.id(), "codex");
        let dir = tempfile::tempdir().unwrap();
        CodexAdapter.provision_hooks(dir.path(), PaneId(1), std::path::Path::new("/tmp/s.sock")).unwrap();
        // provision is a no-op for codex (hook is a launch arg, not a file).
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "codex provision must write nothing");
    }

    #[test]
    fn registry_builds_known_adapters_and_rejects_unknown() {
        assert_eq!(build_adapter("claude").unwrap().id(), "claude");
        assert_eq!(build_adapter("codex").unwrap().id(), "codex");
        assert_eq!(build_adapter("shell").unwrap().id(), "synthetic"); // shell → SyntheticAdapter
        assert!(build_adapter("nope").is_none());
    }

    #[test]
    fn registry_descriptors_list_claude_codex_shell() {
        let ids: Vec<&str> = adapter_descriptors().iter().map(|d| d.id).collect();
        assert!(ids.contains(&"claude") && ids.contains(&"codex") && ids.contains(&"shell"));
    }
}
