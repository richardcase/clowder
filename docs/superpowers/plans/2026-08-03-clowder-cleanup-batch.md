# Cleanup / consolidation batch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove dead code, fix three parallel-load test flakes at their root causes, and make M7d's exposure warning TLS-aware.

**Architecture:** Five small, independent changes: delete `SignalScanner`; fix a real subscribe-after-backlog attention race (product) + a test-loop bug (test) + serialize contention-bound reconcile tests; branch one log line on `remote_tls`.

**Tech Stack:** Rust 2021, `tokio`, `serial_test` (new dev-dep).

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` (rustup not auto-sourced here).
- **Edition 2021, stable.** CI runs `cargo test --workspace --locked` — commit the regenerated `Cargo.lock` when deps change.
- **No feature behavior change.** The only product change is additive: an attaching client now (a) reliably receives later `AttentionChanged` and (b) receives the current attention state on attach.
- **Keep** `AttentionSignal`, `Screen`, `is_blocking_prompt` in `clowder-vt` — only `SignalScanner`/`Collector` go.
- Three known flaky tests this targets: `attached_client_gets_attention_changed`, `client_gets_pane_exited_when_child_exits`, `reconcile_restored_companion_ids_never_collide_with_agents` (+ its sibling `reconcile_respawns_recorded_agents_and_prunes_missing`).

---

### Task 1: Remove dead `SignalScanner`

**Files:**
- Modify: `crates/clowder-vt/src/lib.rs` (delete `SignalScanner`, `Collector`, their tests, now-unused imports)

**Interfaces:**
- Produces: nothing new. `AttentionSignal`, `mod screen`/`pub use screen::Screen`, `mod prompt`/`pub use prompt::is_blocking_prompt` remain exported.

This is a deletion — the "test" is a clean grep + green build.

- [ ] **Step 1: Confirm it is dead**

Run: `grep -rn "SignalScanner" crates | grep -v "clowder-vt/src/lib.rs"`
Expected: **no output** (only the definition/tests in `lib.rs` reference it).

- [ ] **Step 2: Remove**

In `crates/clowder-vt/src/lib.rs`, delete:
- the `pub struct SignalScanner { … }` and its `impl SignalScanner { … }` and `impl Default for SignalScanner { … }`;
- the `struct Collector { … }` and `impl vte::Perform for Collector { … }`;
- the entire `#[cfg(test)] mod tests { … }` block (its tests — `bare_bell_is_a_signal`, `osc9_is_a_notify_not_a_bell`, `osc777_notify_title_and_body`, `title_osc_is_ignored`, `signal_split_across_feeds`, `bells_amid_text_are_each_counted`, `truncated_osc_does_not_panic` — all exercise `SignalScanner`; equivalent coverage lives in `screen.rs`'s signal tests).

Keep: the module doc comment, `pub enum AttentionSignal { … }`, `mod screen; pub use screen::Screen;`, `mod prompt; pub use prompt::is_blocking_prompt;`.

Then remove any import left unused by the deletion (e.g. if `lib.rs` had a top-level `use vte::…` used only by `Collector`, drop it — `AttentionSignal` needs no imports). Let the compiler's `unused_imports` warning guide you.

- [ ] **Step 3: Verify green + clean**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt 2>&1 | tail -10`
Expected: PASS (Screen + prompt suites; the removed signal tests are gone, their behavior still covered by `screen.rs`'s `bell_is_a_signal_and_not_printed` / `osc9_and_osc777_notify` / `title_osc_is_ignored_by_screen`).
Run: `source "$HOME/.cargo/env" && cargo build --workspace 2>&1 | tail -5`
Expected: clean (no external consumer broke; the daemon uses only `Screen`).
Run: `grep -rn "SignalScanner\|struct Collector" crates`
Expected: **no output**.

- [ ] **Step 4: Commit**

```bash
git add crates/clowder-vt/src/lib.rs
git commit -m "refactor(vt): remove dead SignalScanner (superseded by Screen)"
```

---

### Task 2: Fix the attention subscribe-after-backlog race

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (`handle_conn` ~736-742; a new test + confirm the existing one)

**Interfaces:**
- Consumes: `Daemon::subscribe_attention()`, `Daemon::attention_of(PaneId)`, `DaemonToClient::AttentionChanged { pane, state }`.
- Produces: `handle_conn` subscribes to attention **before** sending `Attached`/backlog, and sends the current attention state on attach (if any).

- [ ] **Step 1: Write the new failing test**

Add to the `tests` module in `server.rs` (near `attached_client_gets_attention_changed`):

```rust
#[tokio::test]
async fn attach_to_already_needy_pane_delivers_current_attention() {
    use clowder_proto::AttentionState;
    let daemon = Arc::new(Daemon::new());
    let pane = daemon.spawn_pane(sh("sleep 5"), 80, 24).unwrap();
    // Attention is set BEFORE the client attaches — the client must still learn it.
    daemon.set_attention(pane, AttentionState::NeedsInput);

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let d = daemon.clone();
    tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

    let mut client = MsgStream::<_>::new(client_io);
    client.send(&ClientToDaemon::Attach { pane }).await.unwrap();

    // Within the first few frames after Attach, an AttentionChanged{NeedsInput} must arrive.
    let mut got = None;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await {
            Ok(Ok(Some(DaemonToClient::AttentionChanged { state, .. }))) => { got = Some(state); break; }
            Ok(Ok(Some(_))) => {}                 // Attached / Output
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    assert_eq!(got, Some(AttentionState::NeedsInput), "attaching client must learn current attention");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon attach_to_already_needy_pane_delivers_current_attention 2>&1 | tail -20`
Expected: FAIL — today `handle_conn` never sends the current attention state, so `got` stays `None`.

- [ ] **Step 3: Implement the fix**

In `handle_conn` (`server.rs`), replace the block that currently reads:

```rust
        let (cols, rows) = pane.size();
        msgs.send(&DaemonToClient::Attached { pane: pane.id(), cols, rows }).await?;

        let (snap, mut sub) = pane.snapshot_and_subscribe();
        msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes: snap }).await?;

        let mut att_rx = self.subscribe_attention();
```

with (subscribe first so a change during attach is buffered, not lost; then deliver current attention):

```rust
        let (cols, rows) = pane.size();
        // Subscribe to attention BEFORE sending Attached/backlog: a state change triggered right
        // after the client observes the attach must be buffered by the subscription, not dropped
        // (the old subscribe-after-backlog order lost it under load).
        let mut att_rx = self.subscribe_attention();
        msgs.send(&DaemonToClient::Attached { pane: pane.id(), cols, rows }).await?;
        // Deliver the current attention state so a client attaching to an already-needy agent
        // learns it immediately (future changes still arrive via `att_rx` in the loop below).
        if let Some(state) = self.attention_of(pane.id()) {
            msgs.send(&DaemonToClient::AttentionChanged { pane: pane.id(), state }).await?;
        }

        let (snap, mut sub) = pane.snapshot_and_subscribe();
        msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes: snap }).await?;
```

(The `loop { tokio::select! { … att = att_rx.recv() … } }` below is unchanged.)

- [ ] **Step 4: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon attach_to_already_needy_pane_delivers_current_attention attached_client_gets_attention_changed 2>&1 | tail -20`
Expected: BOTH pass. `attached_client_gets_attention_changed` (a `spawn_pane` pane with no initial attention → the current-state send is skipped) now reliably receives the post-attach `set_attention` via the earlier subscription.
Then run it under repetition to confirm the flake is gone:
Run: `source "$HOME/.cargo/env" && for i in 1 2 3 4 5; do cargo test -p clowder-daemon attached_client_gets_attention_changed 2>&1 | grep -E "test result"; done`
Expected: 5× `ok`.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "fix(daemon): subscribe to attention before backlog; deliver current attention on attach"
```

---

### Task 3: Fix the `PaneExited` poll loop that breaks on a quiet tick

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (`client_gets_pane_exited_when_child_exits` ~1029-1055)

**Interfaces:** test-only.

Root cause: the loop is `if let Ok(Ok(Some(msg))) = timeout(50ms, recv) { … } else { break }` — an elapsed `timeout` (no message yet, stream still open) hits the `else` and breaks early. The sibling `client_gets_final_output_then_pane_exited` (~1057) already uses the correct `match` shape; mirror it.

- [ ] **Step 1: Rewrite the loop**

Replace the loop body of `client_gets_pane_exited_when_child_exits` (currently):

```rust
        let mut exited = false;
        for _ in 0..100 {
            if let Ok(Ok(Some(msg))) =
                tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
            {
                if let DaemonToClient::PaneExited { .. } = msg {
                    exited = true;
                    break;
                }
            } else {
                break; // stream closed
            }
        }
        assert!(exited, "client never received PaneExited on child exit");
```

with (distinguish a quiet tick — keep waiting — from a real close):

```rust
        let mut exited = false;
        for _ in 0..100 {
            match tokio::time::timeout(Duration::from_millis(100), client.recv::<DaemonToClient>()).await {
                Ok(Ok(Some(DaemonToClient::PaneExited { .. }))) => { exited = true; break; }
                Ok(Ok(Some(_))) => {}                 // Attached / Output / AttentionChanged
                Ok(Ok(None)) | Ok(Err(_)) => break,   // stream closed / recv error
                Err(_) => continue,                    // 100ms window elapsed; keep polling
            }
        }
        assert!(exited, "client never received PaneExited on child exit");
```

- [ ] **Step 2: Run it (repeatedly, to confirm the flake is gone)**

Run: `source "$HOME/.cargo/env" && for i in 1 2 3 4 5; do cargo test -p clowder-daemon client_gets_pane_exited_when_child_exits 2>&1 | grep -E "test result"; done`
Expected: 5× `ok`.

- [ ] **Step 3: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "test(daemon): don't break the PaneExited poll loop on a quiet tick"
```

---

### Task 4: Serialize the contention-bound reconcile tests

**Files:**
- Modify: `crates/clowder-daemon/Cargo.toml` (add `serial_test` dev-dep)
- Modify: `crates/clowder-daemon/src/server.rs` (`use serial_test::serial;` + `#[serial]` on two tests)

**Interfaces:** test-only.

- [ ] **Step 1: Add the dev-dependency**

In `crates/clowder-daemon/Cargo.toml` under `[dev-dependencies]`:

```toml
serial_test = "3"
```

- [ ] **Step 2: Annotate the two reconcile tests**

In `server.rs`'s `tests` module, add `use serial_test::serial;` (next to the other `use` lines in the module). Then add `#[serial]` beneath the `#[tokio::test]` on both:

```rust
    #[tokio::test]
    #[serial]
    async fn reconcile_respawns_recorded_agents_and_prunes_missing() { … }
```
and
```rust
    #[tokio::test]
    #[serial]
    async fn reconcile_restored_companion_ids_never_collide_with_agents() { … }
```

(These spawn real `/bin/sh` children + provision `git worktree`s + hold the process-global `STATE_FILE_ENV_LOCK`; `#[serial]` keeps them off the CPU amid peak parallel load. Do not change their bodies.)

- [ ] **Step 3: Verify**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon reconcile 2>&1 | tail -20`
Expected: the reconcile tests pass; `serial_test` compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/clowder-daemon/Cargo.toml crates/clowder-daemon/src/server.rs Cargo.lock
git commit -m "test(daemon): serialize the contention-bound reconcile tests"
```

---

### Task 5: TLS-aware exposure warning

**Files:**
- Modify: `crates/clowder-daemon/src/main.rs` (~64-66)

**Interfaces:**
- Consumes: `config_remote_tls` (the resolved `remote_tls` bool already in scope at this point — it is used a few lines below at the `if config_remote_tls` TLS-build block).

- [ ] **Step 1: Gate the warning on plaintext**

Replace (`main.rs:64-66`):

```rust
        if clowder_daemon::remote::should_warn_exposed(&addr) {
            tracing::warn!(%addr, "remote listener bound to a non-loopback/non-tailnet address — Phase A has NO authentication; expose only behind a trusted tunnel (SSH -L / Tailscale)");
        }
```

with:

```rust
        if clowder_daemon::remote::should_warn_exposed(&addr) {
            if config_remote_tls {
                tracing::info!(%addr, "remote listener bound to a non-loopback/non-tailnet address, protected by TLS + token auth");
            } else {
                tracing::warn!(%addr, "remote listener bound to a non-loopback/non-tailnet address — plaintext with NO authentication; set [remote] tls = true, or expose only behind a trusted tunnel (SSH -L / Tailscale)");
            }
        }
```

- [ ] **Step 2: Verify**

Run: `source "$HOME/.cargo/env" && cargo build -p clowder-daemon 2>&1 | tail -5`
Expected: clean build (`config_remote_tls` resolves — it's the same binding used at the `if config_remote_tls` block just below).
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon exposure 2>&1 | tail -10`
Expected: the existing `should_warn_exposed` predicate test (`exposure_warning_predicate` in `remote.rs`) still passes unchanged (the predicate is untouched; only the caller's messaging changed).

- [ ] **Step 3: Commit**

```bash
git add crates/clowder-daemon/src/main.rs
git commit -m "fix(daemon): make the remote exposure warning TLS-aware"
```

---

## Notes for the implementer

- **Task 2 is the only product change** — additive: subscribe earlier + one conditional `AttentionChanged` on attach. Existing attach tests use `spawn_pane` panes (no attention set → the current-state send is skipped), and all tolerate extra messages (`Ok(Ok(Some(_))) => {}`), so none break.
- **`#[serial]` + `#[tokio::test]`:** put `#[serial]` *below* `#[tokio::test]`; `serial_test` supports async tests. Only the two named reconcile tests get it.
- **Don't touch** `Screen`/`prompt`/`AttentionSignal` (Task 1), the `att_rx` select arm or any other `handle_conn` logic (Task 2), the reconcile test bodies (Task 4), or `should_warn_exposed`'s predicate (Task 5).
- **Whole-suite flake verification** is done by the controller at the finishing gate (`cargo test --workspace --locked` several consecutive times, expecting green with no re-run). Each flake-fix task above also self-verifies its own test 5× — that's the per-task bar.
