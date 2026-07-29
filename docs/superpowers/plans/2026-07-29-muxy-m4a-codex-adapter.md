# muxy M4a — Codex Adapter + Registry + Input-Clear (daemon)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a real **`CodexAdapter`** (launch `codex` with its native `notify` wired to
`muxy-hook` → Completed on turn-complete), an **adapter registry** as the single source
of truth for spawning, and **generalize the M2 input-clear** so any agent whose attention
is `NeedsInput`/`Completed` clears to `Working` on user input (Codex has no Claude-style
"resumed" signal).

**Architecture:** All in `muxy-daemon`. `CodexAdapter` wires the hook via the `codex -c`
launch flag (project `.codex/config.toml` can't set `notify`); `muxy-hook` needs **no**
change (it already ignores Codex's trailing JSON argv, drains stdin, and maps `--event
stop` → `HookKind::Stop` → `Completed`). The registry replaces the hardcoded string match
in `control_json.rs`.

**Tech Stack:** Rust, `anyhow`, tokio; `muxy-daemon` (`agent.rs`, `server.rs`,
`control_json.rs`). Spec: `docs/superpowers/specs/2026-07-29-muxy-m4-codex-adapter-design.md`.

## Global Constraints

- **`muxy-hook` is NOT modified.** Codex fires `notify` only on `agent-turn-complete`,
  invoking `muxy-hook --event stop '<json>'`; muxy-hook's arg loop ignores the trailing
  JSON, drains stdin, and posts `HookKind::Stop` → daemon maps to `Completed`.
- **No `muxy-proto` and no client changes in M4a.** `ListAdapters` is M4b; the picker is M4c.
- Codex hook is wired via the launch flag `codex -c 'notify=["<muxy-hook-bin>","--event","stop"]'`
  (confirmed valid `-c key=value` TOML syntax on codex 0.145.0). The `muxy-hook` path comes
  from the existing `muxy_hook_bin()` resolver. `MUXY_AGENT_ID`/`MUXY_HOOK_SOCK` are already
  pushed onto the agent env by the daemon (`server.rs:128-129`); Codex's `notify` child
  inherits them.
- `anyhow::Result` throughout; mirror the existing `ClaudeAdapter`/`SyntheticAdapter` style.
- Test: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon`.

**Manual validation (not automatable — records into the plan, the USER runs it):** whether
`codex -c 'notify=[...]'` actually *fires* on turn-complete requires a real authenticated
Codex turn. The unit tests assert the adapter's launch shape; the live "notify fires →
Completed badge" check is the user's manual smoke (see Verification). If `-c notify` turns
out to be ignored, the documented fallbacks are global `~/.codex/config.toml` or Codex's
newer `[hooks]` system — but do not build those unless the manual check fails.

---

## Task 1: `CodexAdapter` (muxy-daemon/src/agent.rs)

**Files:**
- Modify: `crates/muxy-daemon/src/agent.rs` (add `CodexAdapter` + tests)
- Modify: `crates/muxy-daemon/src/lib.rs` (export `CodexAdapter`)

**Interfaces:**
- Produces: `CodexAdapter` (impl `AgentAdapter`).

- [ ] **Step 1: Write the failing tests** (in `agent.rs`'s `#[cfg(test)] mod tests`):

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon codex_`
Expected: FAIL — `CodexAdapter` doesn't exist.

- [ ] **Step 3: Implement `CodexAdapter`** (append after `ClaudeAdapter` in `agent.rs`):

```rust
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
```

Add `tempfile` to `muxy-daemon`'s `[dev-dependencies]` only if not already present (it is —
used by existing tests).

- [ ] **Step 4: Export it** in `crates/muxy-daemon/src/lib.rs`:
```rust
pub use agent::{AgentAdapter, ClaudeAdapter, CodexAdapter, SyntheticAdapter};
```

- [ ] **Step 5: Run to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon`
Expected: PASS (new codex tests + all existing).

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon/src/agent.rs crates/muxy-daemon/src/lib.rs
git commit -m "feat(daemon): CodexAdapter wiring codex notify to muxy-hook"
```

---

## Task 2: Adapter registry + rewire spawn (muxy-daemon)

**Files:**
- Modify: `crates/muxy-daemon/src/agent.rs` (add registry)
- Modify: `crates/muxy-daemon/src/lib.rs` (export registry)
- Modify: `crates/muxy-daemon/src/control_json.rs` (spawn via registry)

**Interfaces:**
- Produces: `AdapterDescriptor { id, display_name }`, `adapter_descriptors() -> &'static [AdapterDescriptor]`,
  `build_adapter(id: &str) -> Option<Box<dyn AgentAdapter>>`.
- Consumes: `CodexAdapter` (Task 1).

- [ ] **Step 1: Write the failing tests** (in `agent.rs` tests):

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon registry_`
Expected: FAIL — registry items don't exist.

- [ ] **Step 3: Implement the registry** (in `agent.rs`, after the adapters):

```rust
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
```
Add `use crate::PaneCommand;` if `agent.rs` doesn't already import it (it does — `ClaudeAdapter`
uses `PaneCommand`).

- [ ] **Step 4: Export** in `lib.rs`:
```rust
pub use agent::{adapter_descriptors, build_adapter, AdapterDescriptor};
```

- [ ] **Step 5: Rewire `control_json.rs`** — replace the `match adapter { ... }` in
`spawn_from_control` with the registry:
```rust
    fn spawn_from_control(self: &Arc<Self>, project: &str, task: &str, adapter: &str) -> Result<PaneId> {
        let project_path = Path::new(project);
        let a = build_adapter(adapter).ok_or_else(|| anyhow!("unknown adapter: {adapter}"))?;
        self.spawn_agent(project_path, a.as_ref(), task)
    }
```
Update the `use` at the top of `control_json.rs`: drop `ClaudeAdapter`/`SyntheticAdapter` if now
unused, add `build_adapter` (keep `PaneCommand` only if still referenced elsewhere in the file —
let the compiler guide you). `spawn_agent`'s signature takes `&dyn AgentAdapter`; `a.as_ref()`
yields that from the `Box`.

- [ ] **Step 6: Run to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon`
Expected: PASS — registry tests + the existing `control_json` spawn tests (which spawn `"shell"`)
still pass through the registry unchanged.

- [ ] **Step 7: Commit**

```bash
git add crates/muxy-daemon/src/agent.rs crates/muxy-daemon/src/lib.rs crates/muxy-daemon/src/control_json.rs
git commit -m "feat(daemon): adapter registry (build_adapter/descriptors); spawn via registry"
```

---

## Task 3: Generalize the input-clear (muxy-daemon/src/server.rs)

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (`handle_conn` Input arm + a test)

**Interfaces:**
- Consumes: existing `attention_of`/`set_attention`.

- [ ] **Step 1: Write the failing test** (in `server.rs` tests). A **hook'd** agent (not in
`hookless`) in `Completed` must clear to `Working` on input — this fails under the old
`hookless && NeedsInput` gate. Reuse the existing input-clear test harness
(`input_clears_hookless_needs_input_to_working`, ~line 1132) and the `HookedTestAdapter`
(~line 1098; ensure its `provides_hooks()` returns `true` so the agent is NOT added to
`hookless` — add the method if missing):

```rust
#[tokio::test]
async fn input_clears_hooked_completed_to_working() {
    let (daemon, _tmp) = /* build daemon + temp git repo, as the existing input-clear test does */;
    let agent = daemon.spawn_agent(repo.path(), &HookedTestAdapter { cmd: sleeper() }, "task-a").unwrap();
    // A hook'd agent is NOT in `hookless`.
    daemon.set_attention(agent, AttentionState::Completed);
    // Attach a client and send Input to the agent pane (drives handle_conn's Input arm),
    // exactly like input_clears_hookless_needs_input_to_working does.
    /* ...attach, client.send(Input { pane: agent, bytes: b"x" }) ... */
    // attention must become Working.
    let mut ok = false;
    for _ in 0..50 {
        if daemon.attention_of(agent) == Some(AttentionState::Working) { ok = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(ok, "input to a hook'd Completed agent must clear to Working");
}
```
(Copy the exact daemon/temp-repo/attach boilerplate from `input_clears_hookless_needs_input_to_working`;
only the adapter, the state (`Completed`), and the assertion differ. `sleeper()` = a
`PaneCommand` running `/bin/sh -c 'sleep 30'`, matching other tests.)

- [ ] **Step 2: Run to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon input_clears_hooked_completed`
Expected: FAIL — the old gate only clears `hookless` agents in `NeedsInput`.

- [ ] **Step 3: Generalize the Input arm** in `handle_conn` (`server.rs:~443-446`). Replace:
```rust
                            if self.hookless.lock().unwrap().contains(&pid)
                                && self.attention_of(pid) == Some(AttentionState::NeedsInput)
                            {
                                self.set_attention(pid, AttentionState::Working);
                            }
```
with (drop the `hookless` gate; clear `NeedsInput` OR `Completed` for any agent — non-agent
panes have no attention so `attention_of` is `None` and the guard is false):
```rust
                            // User engaged with an agent whose attention was "waiting" → back to Working.
                            // Applies to all agents: hook-less (VT/BEL) AND hook'd tools like Codex that
                            // only emit a turn-complete signal and no "resumed" event.
                            if matches!(
                                self.attention_of(pid),
                                Some(AttentionState::NeedsInput | AttentionState::Completed)
                            ) {
                                self.set_attention(pid, AttentionState::Working);
                            }
```

- [ ] **Step 4: Run to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon`
Expected: PASS — the new test, the existing `input_clears_hookless_needs_input_to_working`
(still valid: hookless + NeedsInput → Working), and all others.

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): input clears NeedsInput/Completed to Working for all agents"
```

---

## Final verification

- `source "$HOME/.cargo/env" && cargo test -p muxy-daemon` → all green (codex adapter,
  registry, generalized input-clear + all existing).
- Whole-workspace `source "$HOME/.cargo/env" && cargo test` → green (no cross-crate breakage;
  M4a is daemon-only, additive, no proto change).
- **Manual (user, needs an authenticated Codex):** spawn a `codex` agent (once M4c's picker
  lands, or via the free-text field / `muxy spawn <proj> <task> codex`), give it a task; when
  the turn completes the sidebar/tray shows **Completed**; type into the pane → clears to
  **Working**. This confirms `-c notify` fires end-to-end. If it does NOT fire, fall back to
  global config or Codex `[hooks]` (a follow-up), and report it.
