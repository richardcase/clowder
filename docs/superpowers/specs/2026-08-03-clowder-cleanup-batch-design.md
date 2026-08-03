# clowder cleanup / consolidation batch

## Context

After five back-to-back feature milestones (M9a/M9b agent survival, VT grid, M7d remote TLS), a few
concrete debts accumulated that are worth clearing before the next big feature:

1. **Dead `SignalScanner`.** The VT-grid milestone (#52) replaced the daemon's only `SignalScanner` use
   with the new `Screen`. `SignalScanner` (+ its `Collector` `vte::Perform`) is now referenced **only**
   inside `crates/clowder-vt/src/lib.rs` (its definition + its own tests) — dead public API, and its
   OSC 9/777 parsing is duplicated verbatim in `Screen`.
2. **Three flaky daemon tests** that fail only under **parallel `cargo test` load** (pass in isolation +
   on re-run), costing a re-run on nearly every CI/local run across M7/M9/VT/M7d.
3. **M7d's exposure warning is now misleading.** `crates/clowder-daemon/src/main.rs:64` unconditionally
   logs "…Phase A has NO authentication; expose only behind a trusted tunnel" whenever the remote
   listener binds a non-loopback/non-tailnet address — even when `[remote] tls = true` (token auth IS
   active). Flagged as a deferred minor in the M7d review.

### What exists (ground truth, verified 2026-08-03)

- `crates/clowder-vt/src/lib.rs`: `pub struct SignalScanner { parser }`, `impl SignalScanner`,
  `struct Collector: vte::Perform`, and ~7 signal unit tests. `AttentionSignal` (used by `Screen`),
  `mod screen`/`mod prompt` re-exports live in the same file and are **kept**.
- **Attention flake root cause** (`crates/clowder-daemon/src/server.rs`, `handle_conn` ~716-783):
  the handler subscribes to attention (`att_rx = self.subscribe_attention()`, line 742) **after** it
  sends `Attached` (737) and the backlog `Output` (740). A client that flips attention immediately after
  receiving the backlog (exactly what `attached_client_gets_attention_changed` does) can trigger the
  one-shot broadcast **before** line 742 subscribes → the event is lost and the client waits forever.
  Separately, a client attaching to an **already-needy** agent never learns the current attention state
  (only future changes).
- **Exit flake root cause** (`server.rs`, `client_gets_pane_exited_when_child_exits` ~1029-1055): the poll
  loop is `for _ in 0..100 { if let Ok(Ok(Some(msg))) = timeout(50ms, recv) { … } else { break } }` — an
  elapsed 50 ms `timeout` (no message *yet*, stream still open) hits the `else` and **breaks early**. Under
  load the `exit 3` child's `PaneExited` can arrive after the first quiet tick → false failure. The
  sibling `client_gets_final_output_then_pane_exited` (~1058) shares the pattern.
- **Reconcile flakes** (`reconcile_restored_companion_ids_never_collide_with_agents` ~2098,
  `reconcile_respawns_recorded_agents_and_prunes_missing` ~1178): deterministic assertions (per the M9b
  review), but heavy setup — real `/bin/sh` children + `git worktree` provisioning + the process-global
  `STATE_FILE_ENV_LOCK` — makes them contention-sensitive under peak parallel load.
- **Exposure warning** (`main.rs:64`) sits inside the `if let Some(addr_str) = remote_listen` block; the
  resolved `remote_tls` bool is in scope there (`config.remote_tls`, used a few lines below to build TLS).
- `serial_test` is not yet a dependency.

### User decisions (brainstorm, 2026-08-03)

- **Flaky-fix philosophy: targeted per-test fixes** (root-cause each; product fix only where a real race
  exists; `#[serial]` only for the inherently contention-bound real-subprocess tests). Not a blanket
  serialize, not just widening timeouts.
- Fixing the attention delivery **may touch product code** (approved): subscribe before backlog + deliver
  current attention on attach.

## Goals / Non-goals

**Goals:** (1) remove dead `SignalScanner` + its duplicated OSC logic; (2) make the three flaky tests
reliable under parallel load by fixing their root causes (a real product race for attention, a test-logic
bug for exit, contention isolation for reconcile); (3) make the exposure warning accurate under TLS.

**Non-goals:** any new feature; touching `Screen`/`is_blocking_prompt`/`AttentionSignal`; changing the
wire protocol beyond the additive attention-on-attach send; broad test-suite restructuring; fixing
`should_warn_exposed`'s address predicate (it's correct — only its *caller*'s messaging is wrong).

## Component design

### 1. Remove `SignalScanner` (`clowder-vt`)

Delete `SignalScanner`, `impl SignalScanner`, `impl Default for SignalScanner`, `struct Collector` + its
`vte::Perform` impl, and the `SignalScanner` unit tests from `crates/clowder-vt/src/lib.rs`. Keep
`AttentionSignal`, `mod screen`/`pub use screen::Screen`, `mod prompt`/`pub use prompt::is_blocking_prompt`.
Confirm no remaining references (`grep`), then `cargo test -p clowder-vt` (the `Screen`/`prompt` tests are
the remaining coverage — signal detection is exercised via `Screen`'s own signal tests). Net: ~100 fewer
lines, no more OSC-parse duplication.

### 2. Flaky-test fixes (`clowder-daemon`)

- **Attention (product fix).** In `handle_conn`, move `let mut att_rx = self.subscribe_attention();`
  **before** sending `Attached`/backlog (so the subscription exists before the client can observe the
  attach and trigger a change — buffered broadcast, no lost event). Additionally, **after subscribing,
  send the pane's current attention state** if set: `if let Some(state) = self.attention_of(pane.id())
  { msgs.send(&DaemonToClient::AttentionChanged { pane: pane.id(), state }).await?; }` — so a client
  attaching to an already-needy agent learns it immediately. `attached_client_gets_attention_changed`
  becomes deterministic (the post-attach `set_attention` is now always delivered via the pre-existing
  subscription; and the current-state send covers the already-set case).
- **Exit (test-logic fix).** Rewrite the poll loops in `client_gets_pane_exited_when_child_exits` and
  `client_gets_final_output_then_pane_exited` to distinguish a `timeout` elapse (keep waiting) from a
  closed stream (`recv` → `Ok(None)`/`Err`, stop). Only `break` on close or on finding the awaited
  message; a quiet tick must continue the loop. Keep a bounded total wait (e.g. ~5 s) so a genuine hang
  still fails.
- **Reconcile (contention isolation).** Add `serial_test` as a `[dev-dependencies]` of `clowder-daemon`
  and annotate `reconcile_restored_companion_ids_never_collide_with_agents` and
  `reconcile_respawns_recorded_agents_and_prunes_missing` with `#[serial]` so they don't run amid peak
  parallel load. (They already hold `STATE_FILE_ENV_LOCK` for env safety; `#[serial]` addresses the
  CPU/subprocess contention that the mutex doesn't.)

### 3. TLS-aware exposure warning (`clowder-daemon` `main.rs`)

Gate the warning on `!config.remote_tls`: keep the current "NO authentication — expose only behind a
trusted tunnel" `warn!` for a plaintext (`tls` unset) non-loopback/non-tailnet bind (genuinely
dangerous); under `tls = true`, emit a benign `info!` instead (e.g. "remote listener bound to `<addr>`
with TLS + token auth"). `should_warn_exposed(&addr)` (the address predicate) is unchanged.

## Error handling

No new failure modes. The attention-on-attach send uses the existing `msgs.send(...).await?` path (a send
failure ends the session as today). Test changes are test-only except the `handle_conn` reorder + the
one additive attention send.

## Testing

- **`clowder-vt`:** after removal, `cargo test -p clowder-vt` is green (Screen + prompt suites); a repo
  grep shows no `SignalScanner` references remain.
- **Attention:** `attached_client_gets_attention_changed` passes deterministically; add a focused test
  that a client **attaching to an already-`NeedsInput` pane** immediately receives an `AttentionChanged`
  with the current state (covers the new current-state-on-attach path).
- **Exit:** `client_gets_pane_exited_when_child_exits` + `client_gets_final_output_then_pane_exited` pass;
  the loop no longer breaks on a quiet tick (verify by reading — a timeout continues).
- **Reconcile:** the two `#[serial]` tests still pass.
- **Warning:** `should_warn_exposed`'s predicate test is unchanged and green. (The caller's warn-vs-info
  branch is simple enough to verify by reading; no new unit test unless trivial.)
- **Whole suite green under load:** `cargo test --workspace --locked` runs green **without** needing a
  re-run for these three tests (the point of the batch). Run it a few times to confirm the flakes are
  gone.

## Risks

1. **Attention reorder subtlety** — subscribing before sending backlog is safe (broadcast buffers for
   subscribed receivers up to capacity; the handler drains it in the select loop). The additive
   current-state send is idempotent-ish (the client may get one extra `AttentionChanged` equal to current
   state — harmless; the store applies it as a set). Covered by the new test.
2. **`serial_test` dep** — a small, widely-used dev-only crate; pinned via `Cargo.lock`. Only two tests
   serialize, so suite wall-clock impact is negligible.
3. **Residual flakiness** — if a serialized/So-fixed test still flakes, the root cause was mis-diagnosed;
   the fix is per-test and reversible. The batch explicitly targets the three named tests; a fourth
   surfacing later is a separate follow-up.
4. **Removing a `pub` type** (`SignalScanner`) is an API change, but `clowder-vt` is an internal workspace
   lib with a single in-repo consumer (the daemon, which no longer uses it) — no external breakage.

## Verification gate

`SignalScanner` is gone (no references remain; `clowder-vt` green); the three named flaky tests pass
reliably under `cargo test --workspace --locked` across several consecutive runs with no re-run needed;
a client attaching to an already-needy agent immediately sees its attention state; and a `tls=true` remote
bind on a public address no longer logs "NO authentication" (it logs a benign TLS-enabled info line),
while a plaintext public bind still warns. No feature behavior changed.
