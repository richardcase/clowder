# muxy M2 — Attention Fusion (VT-signal fallback) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drive attention for hook-less agents from terminal notification signals: a new `muxy-vt` crate scans a pane's output for BEL / OSC 9 / OSC 777, and the daemon runs that scanner only for agents whose adapter has no hooks, setting `NeedsInput` (cleared to `Working` on user input).

**Architecture:** `muxy-vt` (pure, `vte`-based `SignalScanner`) → the daemon spawns a per-pane scanner task for hook-less agents → `set_attention`. Per-adapter routing (`AgentAdapter::provides_hooks()`) means each agent has exactly one attention source; no client change.

**Tech Stack:** Rust, tokio, the `vte` crate.

## Global Constraints

- **`muxy-vt` is pure** (only depends on `vte`); no daemon/tokio deps. Heavily unit-tested with byte fixtures.
- **Signals = BEL + OSC 9 + OSC 777 only.** Title (OSC 0/1/2), CSI, DCS, print → ignored. An OSC's terminating BEL is the string terminator, not a separate Bell.
- **Scanner runs ONLY for hook-less agents** (`!adapter.provides_hooks()`); hook'd agents are untouched.
- **Debounce = de-dup:** don't re-set `NeedsInput` if already `NeedsInput`.
- **`vte` API drift:** pin the version and match its exact `Perform`/`advance` signatures (they differ across releases) — verify with `cargo doc -p vte` / the crate's docs for the pinned version. The scanner is isolated so drift is contained.
- Commit after each task; conventional messages + standard trailers.

**Test command:** `cargo test` (workspace). Per-crate: `cargo test -p muxy-vt`, `cargo test -p muxy-daemon`.

---

## Task 1: `muxy-vt` crate — signal scanner (new crate)

**Files:**
- Create: `crates/muxy-vt/Cargo.toml`
- Create: `crates/muxy-vt/src/lib.rs` (scanner + `#[cfg(test)]` tests)

(`members = ["crates/*"]` globs, so the new crate is auto-included in the workspace.)

**Interfaces:**
- Produces: `muxy_vt::AttentionSignal`, `muxy_vt::SignalScanner` (`new()`, `feed(&[u8]) -> Vec<AttentionSignal>`).

- [ ] **Step 1: Create `crates/muxy-vt/Cargo.toml`:**

```toml
[package]
name = "muxy-vt"
version = "0.0.0"
edition = "2021"

[dependencies]
vte = "0.13"
```

> If `vte = "0.13"` isn't resolvable or its `Perform`/`advance` API differs from Step 3's code, pin the latest available `0.x` and adapt the `Perform` impl + `feed()` to that version's signatures (verify via `cargo doc -p vte`). Note the version used in the report.

- [ ] **Step 2: Write the failing tests** — in `crates/muxy-vt/src/lib.rs`, a `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_bell_is_a_signal() {
        let mut s = SignalScanner::new();
        assert_eq!(s.feed(b"\x07"), vec![AttentionSignal::Bell]);
    }

    #[test]
    fn osc9_is_a_notify_not_a_bell() {
        let mut s = SignalScanner::new();
        // ESC ] 9 ; hello  BEL   — the trailing BEL terminates the OSC string.
        assert_eq!(
            s.feed(b"\x1b]9;hello\x07"),
            vec![AttentionSignal::Notify { title: String::new(), body: "hello".into() }]
        );
    }

    #[test]
    fn osc777_notify_title_and_body() {
        let mut s = SignalScanner::new();
        // ESC ] 777 ; notify ; Title ; Body  ST(ESC \)
        assert_eq!(
            s.feed(b"\x1b]777;notify;Title;Body\x1b\\"),
            vec![AttentionSignal::Notify { title: "Title".into(), body: "Body".into() }]
        );
    }

    #[test]
    fn title_osc_is_ignored() {
        let mut s = SignalScanner::new();
        assert_eq!(s.feed(b"\x1b]0;my window title\x07"), vec![]);
    }

    #[test]
    fn signal_split_across_feeds() {
        let mut s = SignalScanner::new();
        assert_eq!(s.feed(b"\x1b]9;hel"), vec![]);
        assert_eq!(
            s.feed(b"lo\x07"),
            vec![AttentionSignal::Notify { title: String::new(), body: "hello".into() }]
        );
    }

    #[test]
    fn bells_amid_text_are_each_counted() {
        let mut s = SignalScanner::new();
        assert_eq!(s.feed(b"abc\x07def\x07"), vec![AttentionSignal::Bell, AttentionSignal::Bell]);
    }

    #[test]
    fn truncated_osc_does_not_panic() {
        let mut s = SignalScanner::new();
        let _ = s.feed(b"\x1b]9;partial-no-terminator");   // no panic; no completed signal
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p muxy-vt`
Expected: FAIL — the crate/types don't exist yet.

- [ ] **Step 4: Implement `crates/muxy-vt/src/lib.rs`** (above the test module):

```rust
//! Headless scanner for terminal attention signals (BEL, OSC 9, OSC 777) using the `vte`
//! escape-sequence parser. No cell grid — just signal detection.

/// An attention-worthy signal found in a pane's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttentionSignal {
    Bell,
    Notify { title: String, body: String },
}

/// Feeds PTY output through a `vte` parser and reports attention signals. State persists
/// across `feed` calls, so sequences split across chunk boundaries are detected.
pub struct SignalScanner {
    parser: vte::Parser,
}

impl SignalScanner {
    pub fn new() -> Self {
        Self { parser: vte::Parser::new() }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<AttentionSignal> {
        let mut collector = Collector { out: Vec::new() };
        self.parser.advance(&mut collector, bytes);
        collector.out
    }
}

impl Default for SignalScanner {
    fn default() -> Self { Self::new() }
}

struct Collector {
    out: Vec<AttentionSignal>,
}

impl vte::Perform for Collector {
    fn execute(&mut self, byte: u8) {
        if byte == 0x07 {
            self.out.push(AttentionSignal::Bell);
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params.first() {
            Some(p) if *p == b"9" => {
                let body = params
                    .get(1)
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                self.out.push(AttentionSignal::Notify { title: String::new(), body });
            }
            Some(p) if *p == b"777" => {
                if params.get(1).map(|b| *b == b"notify").unwrap_or(false) {
                    let title = params.get(2).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
                    let body = params.get(3).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
                    self.out.push(AttentionSignal::Notify { title, body });
                }
            }
            _ => {}
        }
    }

    // Everything else is ignored — no-op impls (match the pinned vte's Perform signatures).
    fn print(&mut self, _c: char) {}
    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn csi_dispatch(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}
```

> The `Perform` no-op method signatures (`csi_dispatch`, `hook`, etc.) must match the pinned `vte` version exactly — some releases use `&vte::Params`, others differ. Adjust to what `cargo doc -p vte` shows for the resolved version; only `execute`/`osc_dispatch` carry logic.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p muxy-vt`
Expected: PASS — all 7 tests. (The OSC-9-not-double-counted test is the key correctness case.)

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-vt/
git commit -m "feat(vt): muxy-vt crate — vte-based BEL/OSC9/OSC777 signal scanner"
```

---

## Task 2: `AgentAdapter::provides_hooks()` (muxy-daemon)

**Files:**
- Modify: `crates/muxy-daemon/src/agent.rs` (trait method + impls + test)

**Interfaces:**
- Produces: `AgentAdapter::provides_hooks(&self) -> bool` (`ClaudeAdapter → true`, `SyntheticAdapter → false`).

- [ ] **Step 1: Write the failing test** — in `agent.rs`'s test module:

```rust
    #[test]
    fn adapters_declare_hook_support() {
        assert!(ClaudeAdapter.provides_hooks(), "claude has hooks");
        let synthetic = SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec![], cwd: None, env: vec![] },
        };
        assert!(!synthetic.provides_hooks(), "shell/synthetic has no hooks");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p muxy-daemon adapters_declare_hook_support`
Expected: FAIL — `provides_hooks` doesn't exist.

- [ ] **Step 3: Add the trait method + impls.** In `agent.rs`, add to the `AgentAdapter` trait:

```rust
    /// Whether this adapter injects tool-native attention hooks. If false, the daemon runs the
    /// VT-signal fallback scanner for the agent instead.
    fn provides_hooks(&self) -> bool;
```
Add to `ClaudeAdapter`'s impl:
```rust
    fn provides_hooks(&self) -> bool { true }
```
Add to `SyntheticAdapter`'s impl:
```rust
    fn provides_hooks(&self) -> bool { false }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p muxy-daemon`
Expected: PASS — the new test + all existing (the trait gained a required method; both impls now define it).

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-daemon/src/agent.rs
git commit -m "feat(daemon): AgentAdapter::provides_hooks (claude yes, shell no)"
```

---

## Task 3: Daemon VT-scanner integration + Input-clear (muxy-daemon)

Run the scanner for hook-less agents; clear on input; abort on teardown. Gate: `cargo test`.

**Files:**
- Modify: `crates/muxy-daemon/Cargo.toml` (add `muxy-vt` dep)
- Modify: `crates/muxy-daemon/src/server.rs` (Daemon state + `spawn_agent` scanner task + `handle_conn` input-clear + `teardown_agent` abort + tests)

**Interfaces:**
- Consumes: `muxy_vt::SignalScanner`, `AgentAdapter::provides_hooks` (Task 2), `Pane::{subscribe, id, write_input}`, `Daemon::{set_attention, attention_of}`.

- [ ] **Step 1: Add the dependency.** In `crates/muxy-daemon/Cargo.toml` `[dependencies]`:

```toml
muxy-vt = { path = "../muxy-vt" }
```

- [ ] **Step 2: Write the failing tests** — in `server.rs`'s test module (reuse the existing temp-repo + `Arc<Daemon>` + `SyntheticAdapter` harness that `split_close_and_teardown_manage_the_tree` uses; a hook-less agent is a `SyntheticAdapter`). Also add a tiny test-only hooked adapter:

```rust
    // A test adapter that claims hooks but launches a benign command (so we can assert the
    // scanner is NOT spawned for hook'd agents without needing the `claude` binary).
    struct HookedTestAdapter { cmd: PaneCommand }
    impl crate::agent::AgentAdapter for HookedTestAdapter {
        fn id(&self) -> &'static str { "hooked-test" }
        fn provides_hooks(&self) -> bool { true }
        fn provision_hooks(&self, _w: &std::path::Path, _a: PaneId, _s: &std::path::Path) -> anyhow::Result<()> { Ok(()) }
        fn launch_command(&self, _w: &std::path::Path) -> PaneCommand { self.cmd.clone() }
    }

    fn bell_then_sleep() -> PaneCommand {
        PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "printf '\\a'; sleep 30".into()], cwd: None, env: vec![] }
    }

    #[tokio::test]
    async fn hookless_agent_bell_sets_needs_input() {
        let (daemon, repo) = /* existing harness */;
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter { command: bell_then_sleep() }, "t").unwrap();
        let mut ok = false;
        for _ in 0..100 {
            if daemon.attention_of(agent) == Some(AttentionState::NeedsInput) { ok = true; break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(ok, "a BEL from a hook-less agent should set NeedsInput");
    }

    #[tokio::test]
    async fn hooked_agent_bell_is_ignored() {
        let (daemon, repo) = /* existing harness */;
        let agent = daemon.spawn_agent(repo.path(), &HookedTestAdapter { cmd: bell_then_sleep() }, "t").unwrap();
        // give the BEL time to be produced; attention must stay Working (no scanner).
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(daemon.attention_of(agent), Some(AttentionState::Working));
    }

    #[tokio::test]
    async fn input_clears_hookless_needs_input_to_working() {
        let (daemon, repo) = /* existing harness */;
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter { command: bell_then_sleep() }, "t").unwrap();
        // wait for NeedsInput
        for _ in 0..100 {
            if daemon.attention_of(agent) == Some(AttentionState::NeedsInput) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(daemon.attention_of(agent), Some(AttentionState::NeedsInput));

        // Attach a client and send Input (drives handle_conn's input arm), like the existing
        // client_attaches_and_receives_output test; then assert it clears to Working.
        // … attach over a duplex, send ClientToDaemon::Input { pane: agent, bytes: b"x" } …
        for _ in 0..100 {
            if daemon.attention_of(agent) == Some(AttentionState::Working) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(daemon.attention_of(agent), Some(AttentionState::Working), "input should clear NeedsInput");
    }
```

> Fill the `/* existing harness */` and the attach/send-Input block from the existing
> `server.rs` tests (`split_close_and_teardown_manage_the_tree` for the daemon+repo setup,
> `client_attaches_and_receives_output` for the duplex-attach + `send(ClientToDaemon::Input)`
> pattern). Do not invent new helpers.

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p muxy-daemon hookless`
Expected: FAIL — no scanner yet (attention stays Working).

- [ ] **Step 4: Add Daemon state.** In the `Daemon` struct + `new_with` (`server.rs`):

```rust
    hookless: Arc<Mutex<std::collections::HashSet<PaneId>>>,
    scanners: Arc<Mutex<HashMap<PaneId, tokio::task::JoinHandle<()>>>>,
```
Init both to empty in `new_with` (mirroring the other `Arc<Mutex<HashMap…>>` fields).

- [ ] **Step 5: Spawn the scanner in `spawn_agent`** (after `set_attention(id, Working)`), only for hook-less adapters:

```rust
        if !adapter.provides_hooks() {
            self.hookless.lock().unwrap().insert(id);
            if let Some(pane_arc) = self.panes.lock().unwrap().get(&id).cloned() {
                let me = Arc::clone(self);
                let mut rx = pane_arc.subscribe();
                let handle = tokio::spawn(async move {
                    let mut scanner = muxy_vt::SignalScanner::new();
                    loop {
                        match rx.recv().await {
                            Ok(chunk) => {
                                if !scanner.feed(&chunk).is_empty()
                                    && me.attention_of(id) != Some(AttentionState::NeedsInput)
                                {
                                    me.set_attention(id, AttentionState::NeedsInput);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break, // pane gone
                        }
                    }
                });
                self.scanners.lock().unwrap().insert(id, handle);
            }
        }
```

- [ ] **Step 6: Clear on input in `handle_conn`.** Change the input arm (server.rs:386) so a hook-less agent's `NeedsInput` clears to `Working` when the user sends input:

```rust
                        Some(ClientToDaemon::Input { bytes, .. }) => {
                            let _ = pane.write_input(&bytes);
                            let pid = pane.id();
                            if self.hookless.lock().unwrap().contains(&pid)
                                && self.attention_of(pid) == Some(AttentionState::NeedsInput)
                            {
                                self.set_attention(pid, AttentionState::Working);
                            }
                        }
```

- [ ] **Step 7: Abort the scanner in `teardown_agent`.** Near the existing watcher/tree cleanup:

```rust
        if let Some(h) = self.scanners.lock().unwrap().remove(&pane) { h.abort(); }
        self.hookless.lock().unwrap().remove(&pane);
```

- [ ] **Step 8: Run to verify all pass**

Run: `cargo test -p muxy-daemon` then `cargo test`
Expected: PASS — the three new tests + all existing.

- [ ] **Step 9: Commit**

```bash
git add crates/muxy-daemon/Cargo.toml crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): VT-signal fallback attention for hook-less agents + input-clear"
```

---

## Final verification

- `cargo test` → whole workspace green: `muxy-vt` scanner fixtures, `provides_hooks`, and the daemon hook-less-attention / input-clear / hooked-ignored tests, plus all existing.
- No client change — a hook-less agent that bells now lights the badge + tray count (via the existing `set_attention → AttentionChanged` path) and clears when the user sends input. This completes M2's core (VT-signal fallback fused with hooks by per-adapter routing).
