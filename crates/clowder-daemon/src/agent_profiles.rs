use crate::store::JsonStore;
use anyhow::{bail, Result};
use clowder_config::agents::{merged_profiles, validate_profile, AgentProfile, EffectiveProfile};
use std::path::PathBuf;

/// A profile resolved for spawning: which adapter to build, and the argument template to append
/// once the worktree exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub profile_id: String,
    pub base: String,
    /// Split, NOT yet substituted — the token values only exist after the worktree is provisioned.
    pub arg_template: Vec<String>,
}

/// The built-in `(id, display_name)` pairs, from the one adapter registry.
pub fn builtin_pairs() -> Vec<(&'static str, &'static str)> {
    crate::agent::adapter_descriptors().iter().map(|d| (d.id, d.display_name)).collect()
}

/// The user's agent profiles. Policy-free like `ProjectStore`: it validates and persists, and knows
/// nothing about spawning.
///
/// The file holds only DELTAS — an override row per built-in the user has touched, plus one row per
/// user-created profile. See `merged_profiles`.
pub struct AgentProfileStore {
    store: JsonStore<AgentProfile>,
}

impl AgentProfileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { store: JsonStore::new(path) }
    }

    /// `$CLOWDER_AGENT_PROFILES_FILE` › `$XDG_STATE_HOME/clowder/agent-profiles.json` ›
    /// `$HOME/.local/state/clowder/agent-profiles.json` — the same derivation as the agent
    /// registry and the project store. NOT `agents.json`, which is the live-agent registry.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("CLOWDER_AGENT_PROFILES_FILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        let base = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/state")))
            .unwrap_or_else(|| "/tmp".to_string());
        PathBuf::from(base).join("clowder").join("agent-profiles.json")
    }

    pub fn effective(&self) -> Vec<EffectiveProfile> {
        merged_profiles(self.store.load(), &builtin_pairs())
    }

    fn is_builtin(id: &str) -> bool {
        builtin_pairs().iter().any(|(b, _)| *b == id)
    }

    /// Create a new user profile. Messages are user-facing — they surface in the Settings alert
    /// and on the CLI.
    pub fn add(&self, profile: AgentProfile) -> Result<()> {
        validate_profile(&profile, &builtin_pairs()).map_err(anyhow::Error::msg)?;
        if Self::is_builtin(&profile.id) {
            bail!("{} is a built-in agent — pick another id", profile.id);
        }
        if self.effective().iter().any(|e| e.profile.id == profile.id) {
            bail!("an agent named {} already exists", profile.id);
        }
        self.store.try_mutate(move |all| all.push(profile))?;
        Ok(())
    }

    /// Replace an existing profile — a user row, or a built-in (which writes an override row).
    pub fn update(&self, profile: AgentProfile) -> Result<()> {
        validate_profile(&profile, &builtin_pairs()).map_err(anyhow::Error::msg)?;
        if !self.effective().iter().any(|e| e.profile.id == profile.id) {
            bail!("no such agent: {}", profile.id);
        }
        self.store.try_mutate(move |all| {
            all.retain(|r| r.id != profile.id);
            all.push(profile);
        })?;
        Ok(())
    }

    /// Delete a user profile. Built-ins are never removable — their row is only an override.
    pub fn remove(&self, id: &str) -> Result<()> {
        if Self::is_builtin(id) {
            bail!("{id} is a built-in agent and cannot be removed — disable it instead");
        }
        if !self.effective().iter().any(|e| e.profile.id == id) {
            bail!("no such agent: {id}");
        }
        let id = id.to_string();
        self.store.try_mutate(move |all| all.retain(|r| r.id != id))?;
        Ok(())
    }

    /// Resolve a spawnable id. Validates the template again here: the file is hand-editable and is
    /// not validated on load, so this is the last gate before an agent's argv.
    pub fn resolve(&self, id: &str) -> Result<ResolvedProfile> {
        let e = self
            .effective()
            .into_iter()
            .find(|e| e.profile.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown agent: {id}"))?;
        if !e.profile.enabled {
            bail!("agent {id} is disabled — enable it in Settings, or run `clowder agent enable {id}`");
        }
        clowder_config::agents::validate_template(&e.profile.args).map_err(anyhow::Error::msg)?;
        Ok(ResolvedProfile {
            profile_id: e.profile.id,
            base: e.profile.base,
            arg_template: clowder_config::agents::split_args(&e.profile.args)
                .map_err(anyhow::Error::msg)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, AgentProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = AgentProfileStore::new(dir.path().join("agent-profiles.json"));
        (dir, s)
    }

    fn opus() -> AgentProfile {
        AgentProfile {
            id: "opus".into(),
            base: "claude".into(),
            display_name: "Claude (Opus)".into(),
            enabled: true,
            args: "--model opus".into(),
        }
    }

    #[test]
    fn a_fresh_store_lists_exactly_the_builtins() {
        let (_d, s) = store();
        let ids: Vec<String> = s.effective().into_iter().map(|e| e.profile.id).collect();
        assert_eq!(ids, vec!["claude", "codex", "shell"]);
    }

    #[test]
    fn add_then_effective_includes_the_new_profile() {
        let (_d, s) = store();
        s.add(opus()).unwrap();
        let e = s.effective();
        assert_eq!(e.len(), 4);
        assert_eq!(e[3].profile.id, "opus");
        assert!(!e[3].builtin);
    }

    #[test]
    fn add_rejects_a_duplicate_or_builtin_id() {
        let (_d, s) = store();
        s.add(opus()).unwrap();
        assert!(s.add(opus()).unwrap_err().to_string().contains("already"));
        let mut clash = opus();
        clash.id = "claude".into();
        assert!(s.add(clash).unwrap_err().to_string().contains("built-in"));
    }

    #[test]
    fn add_rejects_an_invalid_profile() {
        let (_d, s) = store();
        let mut bad = opus();
        bad.args = "--x {{nope}}".into();
        assert!(s.add(bad).unwrap_err().to_string().contains("nope"));
    }

    #[test]
    fn update_writes_an_override_row_for_a_builtin() {
        let (_d, s) = store();
        let mut codex = s.effective().into_iter().find(|e| e.profile.id == "codex").unwrap().profile;
        codex.enabled = false;
        s.update(codex).unwrap();
        let e = s.effective();
        assert_eq!(e.len(), 3, "overriding a builtin must not add a row");
        assert!(!e.iter().find(|e| e.profile.id == "codex").unwrap().profile.enabled);
    }

    #[test]
    fn update_rejects_an_unknown_id() {
        let (_d, s) = store();
        let mut ghost = opus();
        ghost.id = "ghost".into();
        assert!(s.update(ghost).unwrap_err().to_string().contains("ghost"));
    }

    #[test]
    fn remove_drops_a_user_profile_and_refuses_a_builtin() {
        let (_d, s) = store();
        s.add(opus()).unwrap();
        s.remove("opus").unwrap();
        assert_eq!(s.effective().len(), 3);

        let e = s.remove("claude").unwrap_err().to_string();
        assert!(e.contains("built-in") && e.contains("disable"), "must point at disable: {e}");
    }

    #[test]
    fn remove_of_an_overridden_builtin_is_still_refused() {
        let (_d, s) = store();
        let mut claude = s.effective().into_iter().next().unwrap().profile;
        claude.args = "--model opus".into();
        s.update(claude).unwrap();
        assert!(s.remove("claude").is_err(), "an override row does not make a builtin removable");
        assert_eq!(s.effective().len(), 3);
    }

    #[test]
    fn resolve_splits_the_template_and_reports_the_base() {
        let (_d, s) = store();
        s.add(opus()).unwrap();
        let r = s.resolve("opus").unwrap();
        assert_eq!(r.base, "claude");
        assert_eq!(r.profile_id, "opus");
        assert_eq!(r.arg_template, vec!["--model", "opus"]);
    }

    #[test]
    fn resolve_rejects_unknown_and_disabled_ids_differently() {
        let (_d, s) = store();
        assert!(s.resolve("ghost").unwrap_err().to_string().contains("unknown"));

        let mut codex = s.effective().into_iter().find(|e| e.profile.id == "codex").unwrap().profile;
        codex.enabled = false;
        s.update(codex).unwrap();
        let e = s.resolve("codex").unwrap_err().to_string();
        assert!(e.contains("disabled"), "{e}");
    }

    #[test]
    fn resolve_rejects_a_hand_edited_bad_template() {
        // agent-profiles.json is hand-editable and is not validated on load, so the spawn path
        // must validate too — a bad token must fail loudly rather than reach an agent's argv.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("agent-profiles.json");
        std::fs::write(
            &p,
            r#"[{"id":"bad","base":"claude","displayName":"Bad","enabled":true,"args":"--x {{nope}}"}]"#,
        )
        .unwrap();
        let s = AgentProfileStore::new(p);
        assert!(s.resolve("bad").unwrap_err().to_string().contains("nope"));
    }

    #[test]
    fn a_corrupt_file_falls_back_to_the_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("agent-profiles.json");
        std::fs::write(&p, b"not json").unwrap();
        assert_eq!(AgentProfileStore::new(p).effective().len(), 3);
    }
}
