//! `clowder agent …` — manage the daemon's agent profiles.
//!
//! `plan` is pure so the argument shapes are tested without a daemon; `run` dials the control
//! socket and prints. The daemon revalidates everything — a remote client is untrusted — but
//! validating here too means a typo fails instantly and locally.

use crate::{
    add_agent_profile_via_control, list_agent_profiles_via_control, remove_agent_profile_via_control,
    update_agent_profile_via_control,
};
use clowder_proto::AgentProfileInfo;

const USAGE: &str = "usage: clowder agent <list|add|set|enable|disable|rm> ...\n\
    \x20 clowder agent list\n\
    \x20 clowder agent add <id> --base <claude|codex|shell> [--name <s>] [--args \"<template>\"] [--disabled]\n\
    \x20 clowder agent set <id> [--name <s>] [--args \"<template>\"]\n\
    \x20 clowder agent enable <id>\n\
    \x20 clowder agent disable <id>\n\
    \x20 clowder agent rm <id>";

#[derive(Debug)]
pub enum Action {
    List,
    Add(AgentProfileInfo),
    Update(AgentProfileInfo),
    Remove(String),
}

/// The base agents a new profile may wrap, as the daemon currently reports them. Derived rather
/// than hardcoded so it cannot drift from the daemon's adapter registry.
fn builtin_ids(existing: &[AgentProfileInfo]) -> Vec<&str> {
    existing.iter().filter(|p| p.builtin).map(|p| p.id.as_str()).collect()
}

/// Decide what `args` means against the daemon's current profiles. Pure.
pub fn plan(args: &[String], existing: &[AgentProfileInfo]) -> Result<Action, String> {
    let sub = args.first().map(|s| s.as_str()).ok_or_else(|| USAGE.to_string())?;
    // Parse the whole tail: `parse_flags` consumes each value flag's value, so the id is simply
    // the first positional — and `agent add --base claude plan` works as well as the usual order.
    let flags = crate::remote_cli::parse_flags(&args[1..])?;
    if sub == "list" {
        flags.reject_unknown(&[])?;
        return Ok(Action::List);
    }
    let id = flags
        .positional(0)
        .map(str::to_string)
        .ok_or_else(|| format!("{USAGE}\n\nmissing <id>"))?;

    let find = |id: &str| {
        existing
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| format!("no such agent: {id}"))
    };

    // A display name the daemon would reject. `trim` matches `validate_profile`, which also counts
    // an all-whitespace name as empty.
    let checked_name = |name: &str| -> Result<String, String> {
        if name.trim().is_empty() {
            return Err("display name must not be empty".into());
        }
        Ok(name.to_string())
    };

    match sub {
        "add" => {
            flags.reject_unknown(&["base", "name", "args", "disabled"])?;
            clowder_config::agents::validate_id(&id)?;
            if existing.iter().any(|p| p.id == id) {
                return Err(format!("an agent named {id} already exists"));
            }
            let base = flags
                .str("base")
                .ok_or_else(|| "add requires --base <claude|codex|shell>".to_string())?
                .to_string();
            // Check it against the built-ins the DAEMON just reported, not a list hardcoded here —
            // a hardcoded one would drift the moment an adapter is added. Keeps the "fails
            // instantly and locally" promise instead of spending a round trip to be told no.
            if !existing.iter().any(|p| p.builtin && p.id == base) {
                return Err(format!(
                    "unknown agent {base:?} — must be one of {}",
                    builtin_ids(existing).join(", ")
                ));
            }
            let args_template = flags.str("args").unwrap_or_default().to_string();
            clowder_config::agents::validate_template(&args_template)?;
            Ok(Action::Add(AgentProfileInfo {
                display_name: checked_name(flags.str("name").unwrap_or(&id))?,
                id,
                base,
                enabled: !flags.bool("disabled"),
                args: args_template,
                builtin: false,
            }))
        }
        "set" => {
            flags.reject_unknown(&["name", "args"])?;
            let mut p = find(&id)?;
            if let Some(name) = flags.str("name") {
                p.display_name = checked_name(name)?;
            }
            if let Some(a) = flags.str("args") {
                clowder_config::agents::validate_template(a)?;
                p.args = a.to_string();
            }
            Ok(Action::Update(p))
        }
        "enable" | "disable" => {
            flags.reject_unknown(&[])?;
            let mut p = find(&id)?;
            p.enabled = sub == "enable";
            Ok(Action::Update(p))
        }
        "rm" => {
            flags.reject_unknown(&[])?;
            let p = find(&id)?;
            if p.builtin {
                return Err(format!(
                    "{id} is a built-in agent and cannot be removed — run `clowder agent disable {id}` instead"
                ));
            }
            Ok(Action::Remove(id))
        }
        other => Err(format!("{USAGE}\n\nunknown subcommand: {other}")),
    }
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let sock = clowder_config::Config::load().control_sock;
    let existing = list_agent_profiles_via_control(&sock).await?;
    let action = plan(args, &existing).map_err(anyhow::Error::msg)?;
    let profiles = match action {
        Action::List => existing,
        Action::Add(p) => add_agent_profile_via_control(&sock, p).await?,
        Action::Update(p) => update_agent_profile_via_control(&sock, p).await?,
        Action::Remove(id) => remove_agent_profile_via_control(&sock, &id).await?,
    };
    for p in profiles {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            p.id,
            p.base,
            if p.enabled { "enabled" } else { "disabled" },
            p.display_name,
            p.args
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn existing() -> Vec<clowder_proto::AgentProfileInfo> {
        vec![
            // All three built-ins, as the daemon always reports them — `--base` is now validated
            // against exactly this list, so a fixture missing one would test a state that cannot
            // occur.
            clowder_proto::AgentProfileInfo {
                id: "claude".into(), base: "claude".into(), display_name: "Claude Code".into(),
                enabled: true, args: String::new(), builtin: true,
            },
            clowder_proto::AgentProfileInfo {
                id: "codex".into(), base: "codex".into(), display_name: "OpenAI Codex".into(),
                enabled: true, args: String::new(), builtin: true,
            },
            clowder_proto::AgentProfileInfo {
                id: "shell".into(), base: "shell".into(), display_name: "Shell".into(),
                enabled: true, args: String::new(), builtin: true,
            },
            clowder_proto::AgentProfileInfo {
                id: "opus".into(), base: "claude".into(), display_name: "Claude (Opus)".into(),
                enabled: true, args: "--model opus".into(), builtin: false,
            },
        ]
    }

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn add_builds_a_profile_from_flags() {
        let a = args("add plan --base claude --name Planner --args --permission-mode=plan");
        match plan(&a, &existing()).unwrap() {
            Action::Add(p) => {
                assert_eq!((p.id.as_str(), p.base.as_str()), ("plan", "claude"));
                assert_eq!(p.display_name, "Planner");
                assert_eq!(p.args, "--permission-mode=plan");
                assert!(p.enabled);
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn add_defaults_the_display_name_to_the_id_and_honours_disabled() {
        match plan(&args("add plan --base shell --disabled"), &existing()).unwrap() {
            Action::Add(p) => {
                assert_eq!(p.display_name, "plan");
                assert!(!p.enabled);
                assert_eq!(p.args, "");
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn add_requires_a_base_and_a_valid_id_and_valid_args() {
        assert!(plan(&args("add plan"), &existing()).is_err(), "--base is required");
        // Built explicitly, NOT via `args()`: that helper splits on whitespace, so "has\ space"
        // would arrive as two argv entries and this would pass without ever testing an id that
        // actually contains a space — which is what a real shell would deliver from `'has space'`.
        let spaced = ["add", "has space", "--base", "claude"].map(String::from).to_vec();
        let e = plan(&spaced, &existing()).unwrap_err();
        assert!(e.contains("letters") || e.contains("may only contain"), "must explain: {e}");
        let e = plan(&args("add p --base claude --args {{nope}}"), &existing()).unwrap_err();
        assert!(e.contains("nope"), "args are validated before the daemon is dialled: {e}");
    }

    #[test]
    fn add_rejects_a_base_that_is_not_a_builtin() {
        let e = plan(&args("add p --base emacs"), &existing()).unwrap_err();
        assert!(e.contains("emacs"), "must name the offender: {e}");
        assert!(e.contains("claude") && e.contains("codex"), "must list the valid bases: {e}");

        // A USER profile is not a valid base either — only the built-ins are.
        assert!(plan(&args("add p --base opus"), &existing()).is_err());
    }

    #[test]
    fn a_blank_display_name_is_refused_before_the_daemon_is_dialled() {
        // The daemon rejects these; catching them here keeps the error local.
        for blank in ["", "   "] {
            let add = ["add", "p", "--base", "claude", "--name", blank].map(String::from).to_vec();
            assert!(plan(&add, &existing()).is_err(), "add --name {blank:?} must be refused");

            let set = ["set", "opus", "--name", blank].map(String::from).to_vec();
            assert!(plan(&set, &existing()).is_err(), "set --name {blank:?} must be refused");
        }
    }

    #[test]
    fn set_patches_only_the_flags_given() {
        match plan(&args("set opus --name Opus"), &existing()).unwrap() {
            Action::Update(p) => {
                assert_eq!(p.display_name, "Opus");
                assert_eq!(p.args, "--model opus", "an untouched field keeps its stored value");
                assert!(p.enabled);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn enable_and_disable_flip_only_that_field() {
        match plan(&args("disable opus"), &existing()).unwrap() {
            Action::Update(p) => {
                assert!(!p.enabled);
                assert_eq!(p.args, "--model opus");
                assert_eq!(p.display_name, "Claude (Opus)");
            }
            other => panic!("expected Update, got {other:?}"),
        }
        match plan(&args("enable opus"), &existing()).unwrap() {
            Action::Update(p) => assert!(p.enabled),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn set_and_rm_reject_an_unknown_id() {
        assert!(plan(&args("set ghost --name x"), &existing()).unwrap_err().contains("ghost"));
        assert!(plan(&args("rm ghost"), &existing()).unwrap_err().contains("ghost"));
    }

    #[test]
    fn rm_refuses_a_builtin_locally_with_the_same_advice_as_the_daemon() {
        let e = plan(&args("rm claude"), &existing()).unwrap_err();
        assert!(e.contains("built-in") && e.contains("disable"), "{e}");
    }

    #[test]
    fn list_needs_no_id() {
        assert!(matches!(plan(&args("list"), &existing()).unwrap(), Action::List));
    }

    #[test]
    fn list_rejects_an_unknown_flag_instead_of_ignoring_it() {
        let e = plan(&args("list --typo"), &existing()).unwrap_err();
        assert!(e.contains("unknown flag"), "{e}");
    }

    #[test]
    fn an_unknown_subcommand_reports_usage() {
        assert!(plan(&args("frobnicate"), &existing()).unwrap_err().contains("usage"));
        assert!(plan(&[], &existing()).unwrap_err().contains("usage"));
    }
}
