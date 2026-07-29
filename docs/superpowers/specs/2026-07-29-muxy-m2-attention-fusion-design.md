# muxy M2 — Attention Fusion (VT-signal fallback)

## Context

muxy's attention detection is **hook-primary**: Claude Code's `Notification`/`Stop`/(injected
`Active`) hooks drive each agent's `AttentionState` (M0/M1c-3). Tools **without** hooks —
today the `shell` adapter, later codex/aider/goose (M4) — currently show no attention. **M2
adds the fallback:** scan a hook-less agent's terminal output for explicit notification
signals (BEL, OSC 9, OSC 777) and drive attention from them, so any tool that rings the bell
or emits a desktop-notification escape gets a sidebar badge + tray count.

Brainstormed & approved decisions:
- **Signal scanner only, not a full VT grid.** A new `muxy-vt` crate uses the `vte` crate
  (alacritty's escape-sequence *parser*) to detect signals — no authoritative cell grid.
  (The full grid — exact-screen snapshots, correctness-while-detached, phone substrate — is
  deferred to M6; snapshot-on-attach already works via the byte-tail backlog.)
- **Signals = BEL + OSC 9 + OSC 777.** Explicit "notify the user" sequences. Title changes
  (OSC 0/2) are parsed and ignored (too noisy). No output-idle / prompt-regex heuristics.
- **Per-adapter fallback fusion.** The scanner drives attention **only for agents whose
  adapter doesn't provide hooks**; hook'd agents (Claude) stay hook-only. Each agent's
  attention thus has exactly one source — no conflict-resolution logic.

### What exists (ground truth)

`muxy-daemon`: `AgentAdapter` trait (`id`, `provision_hooks`, `launch_command`) — `ClaudeAdapter`
(has hooks), `SyntheticAdapter` (no hooks). `Pane` broadcasts output via `output_tx` /
`subscribe() -> broadcast::Receiver<Vec<u8>>`. `Daemon::set_attention(pane, state)` (broadcasts
+ notifies) / `attention_of(pane) -> Option<AttentionState>`. `handle_hook_conn` maps
`HookKind → AttentionState`. `spawn_agent` registers the pane, `set_attention(Working)`, and
spawns a reaper watcher (in the `watchers` map). `handle_conn` input loop:
`ClientToDaemon::Input { bytes } => pane.write_input(&bytes)` (server.rs:386). `teardown_agent`
aborts the watcher + removes pane/tree/etc. `AttentionState {idle, working, needsInput,
completed, exited}`. No workspace VT deps yet; no `muxy-vt` crate.

## Goals / Non-goals

**Goals:** a hook-less agent that emits a BEL / OSC 9 / OSC 777 → its attention becomes
`NeedsInput` (badge + tray count); when the user then sends input to that pane, it clears back
to `Working`. A robust, unit-tested `vte`-based scanner. Hook'd agents are unaffected.

**Non-goals (deferred):** the authoritative VT grid (M6); title changes as attention;
output-idle/prompt-regex heuristics; per-project attention rollups (global count already
exists from M1d; per-project waits for a display — YAGNI); OS-notification wiring beyond the
existing `set_attention → notifier` path.

## Component design

### `muxy-vt` crate (pure, unit-tested)

A signal scanner over the `vte` parser. Pin `vte` in the workspace; match its `Perform`/parser
API for the pinned version.

```rust
pub enum AttentionSignal {
    Bell,
    Notify { title: String, body: String },
}

pub struct SignalScanner { /* holds a vte::Parser (stateful across feeds) */ }

impl SignalScanner {
    pub fn new() -> Self;
    /// Feed a chunk of PTY output; return the attention signals found in it. The parser state
    /// persists across calls, so sequences split across chunk boundaries are detected.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<AttentionSignal>;
}
```

Detection (via a `vte::Perform` impl that pushes into a per-feed buffer):
- **BEL** → `Perform::execute(0x07)` in ground state → `AttentionSignal::Bell`. (A BEL that
  *terminates* an OSC string is consumed by the parser as ST, not re-counted — so
  `OSC 9;msg BEL` yields one `Notify`, not a `Notify` + a `Bell`.)
- **OSC 9** (`ESC ] 9 ; <message> BEL|ST`) → `osc_dispatch` with `params[0] == b"9"` →
  `Notify { title: "", body: <message> }`.
- **OSC 777** (`ESC ] 777 ; notify ; <title> ; <body> ST`) → `osc_dispatch` with
  `params[0] == b"777"` and `params[1] == b"notify"` → `Notify { title, body }`.
- Everything else (print, CSI, OSC 0/1/2 title, DCS, …) → no-op / ignored.

**Tests:** a bare BEL → `[Bell]`; `OSC 9;hello BEL` → `[Notify{body:"hello"}]` and NOT a Bell;
`OSC 777;notify;T;B ST` → `[Notify{title:"T",body:"B"}]`; a title `OSC 0;xyz BEL` → `[]`; a
sequence split across two `feed()` calls still detected; bells interleaved with normal text
counted correctly; malformed/truncated OSC doesn't panic.

### Daemon integration (`muxy-daemon`)

**`AgentAdapter::provides_hooks(&self) -> bool`** — new trait method. `ClaudeAdapter → true`;
`SyntheticAdapter → false`. (Required method; future adapters declare explicitly.)

**Per-pane scanner task (hook-less agents only).** In `spawn_agent`, after registering the
pane and `set_attention(Working)`, if `!adapter.provides_hooks()`:
- record the agent as hook-less (a `hookless: Mutex<HashSet<PaneId>>` on `Daemon`);
- spawn a task that `pane.subscribe()`s the output broadcast and, for each chunk, runs
  `SignalScanner::feed`; on **any** signal (Bell or Notify), if the agent isn't already
  `NeedsInput`, `set_attention(agent, NeedsInput)` — the **de-dup is the debounce** (a burst
  of bells is one state change);
- store the task handle (a `scanners: Mutex<HashMap<PaneId, JoinHandle<()>>>`) so
  `teardown_agent` can `abort()` it (alongside the existing watcher cleanup).

Hook'd agents run **no** scanner — their attention is hook-driven, so there's no source
conflict and no fusion-priority logic.

**Input clears to Working (the VT analog of the `Active` hook).** In `handle_conn`'s input
arm (`ClientToDaemon::Input`), after writing to the pane: if the pane is a hook-less agent
(`hookless` contains it) and its attention is `NeedsInput`, `set_attention(pane, Working)` —
so a BEL-driven `NeedsInput` clears when the user deals with it instead of sticking.

## Data flow

```
hook'd agent (Claude):   hook ─► set_attention                       (unchanged; no scanner)
hook-less agent (shell): pane output chunk ─► SignalScanner.feed ─► Bell/Notify
                              ─► if not already NeedsInput: set_attention(NeedsInput)
                         client Input to that pane ─► if hookless & NeedsInput: set_attention(Working)
teardown_agent ─► abort the scanner task + drop the hookless entry (+ existing cleanup)
```

Both paths converge on the existing `set_attention` → `attention_tx` broadcast + `notifier` +
the control-channel `AttentionChanged` the client already renders (badge + tray count). No
client change.

## Testing

- **`muxy-vt` (`cargo test`):** the scanner fixture tests above.
- **`muxy-daemon` (`cargo test`):** `provides_hooks()` per adapter; a hook-less agent
  (`SyntheticAdapter` running a shell that emits a BEL, e.g. `printf '\\a'`) → `attention_of`
  becomes `NeedsInput`; then delivering an `Input` for that pane → `Working`; a hook'd agent's
  BEL does **not** change attention (no scanner spawned); teardown aborts the scanner (no
  leaked task / no attention change after teardown). Use the existing temp-repo + `Arc<Daemon>`
  test harness.

## Risks

1. **BEL is the noisiest signal.** A shell can BEL for benign reasons (tab-complete miss,
   Ctrl-G). Mitigated by de-dup (one state change per burst) + Input-clear (the user clears it
   by interacting). OSC 9/777 are low-false-positive. Acceptable per the north-star (BEL is
   listed); revisit if it proves noisy.
2. **`vte` API drift across versions.** Pin the version and match its `Perform`/`advance`
   signature exactly (byte-slice vs byte-at-a-time differs across releases). The scanner is
   isolated in `muxy-vt`, so drift is contained.
3. **Broadcast lag on a chatty pane.** The scanner is another `output_tx` subscriber; if it
   lags, `recv()` returns `Lagged` — treat as "continue" (a dropped chunk at worst misses a
   signal, self-corrects on the next). Don't let a lagged scanner break.
4. **Task lifetime.** The scanner task holds an `Arc<Daemon>` (weak-ish via the task) and a
   receiver; `teardown_agent` must `abort()` it so it doesn't outlive the pane.

## Verification gate

`cargo test` green across the workspace (new `muxy-vt` scanner tests + the daemon
hook-less-attention/Input-clear/teardown tests + all existing). No client change; verified by
the daemon tests and (manually, later) a hook-less agent that bells showing a badge + tray
count that clears on input. This completes M2's core (VT-signal fallback fused with hooks by
per-adapter routing).
