# clowder VT grid — headless screen + content-based attention

## Context

`clowder-vt` today is a **signal-only** scanner: it feeds a pane's PTY output through the `vte`
escape-sequence parser and emits `AttentionSignal`s (`Bell` on BEL, `Notify` on OSC 9 / OSC 777). Its
`vte::Perform` impl no-ops `print`, `csi_dispatch`, cursor moves, and everything else — there is **no cell
grid**. Attention routing is otherwise driven by agent lifecycle **hooks**.

This milestone builds a headless **screen buffer** (a parsed visible-screen cell grid) and uses it to
**harden attention routing**: detect that a hookless agent is *actually blocked at an interactive prompt*
— a case BEL/OSC and hooks miss today.

### User decisions (brainstorm, 2026-08-03)

- **Primary use case: content-based attention (b).** Detect a waiting agent from rendered screen content.
- **Secondary: M9c survival substrate (d).** Keep the grid snapshottable so a future M9c re-attach can
  restore the visible screen; **no M9c work in this milestone**.
- **Grid scope: visible screen only (`rows × cols`)** — no scrollback ring. The raw-byte backlog
  (`CLOWDER_BACKLOG_CAP`) already exists for re-render; parsed scrollback/search (use case c) is out.
- **Attention trigger: curated blocking-prompt patterns at quiescence.** After output goes idle, flag
  `NeedsInput` only when the last non-empty line matches a known *interactive* prompt; bare shell prompts
  are excluded (an idle shell must NOT read as needing attention).
- **Which panes: hookless agents only** (`shell` / `synthetic`). Hook-based agents (`claude` / `codex`)
  are unchanged. The `Screen` type is built reusable so a later milestone / M9c can extend it to all panes.

### What exists (ground truth, verified 2026-08-03)

- `crates/clowder-vt/src/lib.rs`: `SignalScanner { parser: vte::Parser }`, `feed(&mut self, &[u8]) ->
  Vec<AttentionSignal>`, and a `Collector: vte::Perform` that handles only `execute` (BEL) and
  `osc_dispatch` (OSC 9 / 777); `print`/`csi_dispatch`/`hook`/… are no-ops. `vte 0.13`'s
  `Parser::advance` takes **one byte at a time**. Dep: `vte = "0.13"` only (no `unicode-width`).
- `crates/clowder-daemon/src/server.rs` `finalize_agent` (~lines 268–297): **only** for
  `!adapter.provides_hooks()` agents, it `snapshot_and_subscribe()`s the pane, feeds a `SignalScanner`,
  and `set_attention(id, NeedsInput)` when a signal appears; the loop runs `rx.recv()` in a `tokio::spawn`
  and is stored in `self.scanners`.
- `AttentionState` (`crates/clowder-proto/src/message.rs:41`): `Idle`, `Working`, `NeedsInput`,
  `Completed`, `Exited`. `finalize_agent` sets `Working` at spawn; the hookless scanner escalates to
  `NeedsInput`.
- Panes track size: `Pane::size() -> (u16, u16)`, `Pane::resize(cols, rows)`; the daemon applies client
  `ClientToDaemon::Resize { pane, cols, rows }` at `server.rs:739` (`pane.resize(cols, rows)`).
- `Pane::snapshot_and_subscribe()` returns the current backlog snapshot + a broadcast receiver of
  subsequent output chunks (`Vec<u8>`); `Lagged` is currently skipped, terminal errors break the loop.

## Goals / Non-goals

**Goals:** (1) a headless, resizable **visible-screen cell grid** (`Screen`) in `clowder-vt`, driven by
the `vte` parser, tracking glyphs + cursor + alt-screen state; (2) a single `vte::Perform` that maintains
the grid **and** emits the existing `AttentionSignal`s (one parse pass); (3) **content-based attention**:
a hookless agent blocked at a curated interactive prompt (at quiescence, not in alt-screen) is flagged
`NeedsInput`, self-clearing when output resumes; (4) never panic on any byte sequence; (5) `Screen`
exposes a text `snapshot()` for later M9c reuse.

**Non-goals:** parsed **scrollback** / cross-pane search (c); **SGR color/style** fidelity (chars-only
grid); a pixel-perfect emulator (scroll regions, origin mode, sixel); grids for **hook-based** agents;
any **wire-protocol or macOS-app** change; any **M9c** re-attach/survival wiring.

## Component design

### 1. `Screen` — the visible-screen cell grid (`clowder-vt`)

A new `Screen` type owns a `vte::Parser` and the parsed grid. Public API (superset of `SignalScanner`):

```rust
pub struct Screen { /* parser + grid + cursor + alt-screen flag + pending signals */ }

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self;
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<AttentionSignal>; // drives the grid AND returns signals
    pub fn resize(&mut self, cols: u16, rows: u16);               // best-effort: re-alloc + clear
    pub fn cursor(&self) -> (u16, u16);                           // (col, row), 0-based
    pub fn line(&self, row: u16) -> String;                       // row's glyphs, trailing blanks trimmed
    pub fn last_nonempty_line(&self) -> String;                   // bottom-most non-blank row (or "")
    pub fn is_alt_screen(&self) -> bool;
    pub fn snapshot(&self) -> Vec<String>;                        // all rows top→bottom (for M9c later)
}
```

- **Grid representation:** `rows` lines of `cols` `char` cells (chars only — no attributes). Wide chars
  (`unicode_width::UnicodeWidthChar::width`): a width-2 glyph occupies its cell and blanks the next;
  width-0 (combining) glyphs are dropped this milestone (documented). Blank cell = `' '`.
- **Signals folded in:** `Screen`'s single `vte::Perform` does the grid work in `print`/`csi_dispatch`/
  `execute`/etc. AND pushes `AttentionSignal`s from `execute` (BEL) and `osc_dispatch` (OSC 9/777) —
  reusing today's exact detection logic. `feed` drains and returns them.
- `SignalScanner` stays in the crate unchanged (still used elsewhere / for signal-only callers); the
  daemon's hookless path switches to `Screen`.

### 2. VT coverage — what the parser tracks

Bounded to "correct enough for the bottom-of-screen prompt + cursor":

- **`print(c)`**: place glyph at cursor, advance cursor by the glyph width; at the right margin, **autowrap**
  to column 0 of the next line (scrolling if on the last row).
- **`execute(byte)`**: `LF (0x0A)` → cursor down one row, scrolling the grid up if on the last row;
  `CR (0x0D)` → cursor col 0; `BS (0x08)` → cursor left one (clamped); `HT (0x09)` → next multiple-of-8
  column (clamped); `BEL (0x07)` → push `Bell` signal (unchanged).
- **`csi_dispatch`**: cursor `CUU`/`CUD`/`CUF`/`CUB` (`A`/`B`/`C`/`D`), `CUP`/`HVP` (`H`/`f`, row;col
  1-based → 0-based, clamped), `CHA` (`G`, column), `VPA` (`d`, row); erase `ED` (`J`: 0=cursor→end,
  1=start→cursor, 2/3=all) and `EL` (`K`: 0/1/2) writing blanks; `IND`-like scrolling handled via LF.
  `SGR` (`m`) and unrecognized finals are **no-ops** (chars-only grid).
- **`esc_dispatch`**: `IND` (`D`) → LF-equivalent; `RI` (`M`) → cursor up, scrolling **down** if on the
  top row; `NEL` (`E`) → CR+LF.
- **Alternate screen:** `csi_dispatch` with the `?` private intermediate and final `h`/`l` for params
  `1049` (and `1047`/`47`) toggles `alt_screen`. On **enter**, save nothing and **clear** the grid to
  blanks (so prior normal-screen content can't be mistaken for a prompt); on **leave**, clear again
  (real content repaints via subsequent output / the raw-byte re-render path). `is_alt_screen()` reports
  the flag.
- **Resize:** `Screen::resize(cols, rows)` re-allocates the grid to the new size and clears it, clamping
  the cursor; subsequent output repaints (shells redraw their prompt on `SIGWINCH`). The daemon calls it
  from the `Resize` path (below).
- **Explicitly out:** scroll regions (`DECSTBM` — treated as whole-screen scroll), origin mode, SGR
  colors/styles, sixel/graphics, width-0 combining marks. All unrecognized sequences are safe no-ops;
  **no byte input may panic** (clamp every index).

### 3. Content-based attention + daemon integration

- **Curated blocking-prompt matcher** (pure fn in `clowder-vt`, e.g. `is_blocking_prompt(line: &str) ->
  bool`): true when the trimmed last-non-empty line matches a known interactive prompt (case-insensitive):
  - trailing `(y/n)`, `(yes/no)`, `[y/n]`, `[Y/n]`, `[y/N]` (optionally followed by `?`/`:`/space);
  - `password:` / `passphrase` (…for…) endings;
  - `press enter` / `press any key` (to continue);
  - pager `--More--`, `(END)`;
  - inquirer-style leading `? ` with a trailing prompt;
  - REPL `>>> ` (Python), `In [<n>]:` (IPython).
  - **Excluded** (return false): bare shell prompts ending in `$ `, `% `, `# `, `❯ `, `> ` with no
    preceding interactive token. The exact regex/string set is enumerated in the plan and pinned by tests.
- **Quiescence in the daemon's hookless scanner** (`finalize_agent`): replace `SignalScanner` with a
  `Screen` sized from `pane.size()`. The scan loop becomes a `tokio::select!` over (a) the output
  broadcast receiver and (b) a `tokio::time::sleep` idle timer (~500 ms, a module const). On each output
  chunk: `screen.feed(chunk)` → any returned `AttentionSignal` still escalates to `NeedsInput`
  immediately (unchanged); then **re-arm** the idle timer. Output does not itself change attention state
  (matching today's model, where only client input clears `NeedsInput`). On idle-timer fire: if
  `!screen.is_alt_screen()` and `is_blocking_prompt(&screen.last_nonempty_line())`, `set_attention(id,
  NeedsInput)`.
- **State rule (avoid flicker):** new output re-arms the idle timer but does not change attention state.
  Content-attention only ever *sets* `NeedsInput` (at quiescence); it never downgrades
  `Working`/`Completed`/`Exited` and never fights a hook (hookless agents have no hooks). A content-set
  `NeedsInput` clears the same way every waiting state does today: the existing **client-input** path
  (`server.rs:732-736`) resets `NeedsInput`/`Completed` → `Working` when the user answers the prompt.
  BEL/OSC keep firing immediately regardless of the timer. (A prompt that vanishes on its own without
  input is a rare edge; the stale `NeedsInput` is acceptable and still clears on the next input.)
- **Resize wiring:** at `server.rs:739`'s `Resize` handler, after `pane.resize`, also resize the pane's
  `Screen` if it has a scanner. The scanner owns the `Screen`; the simplest wiring is for the scan loop to
  observe size via a shared handle or to re-read `pane.size()` on each idle tick and `screen.resize` when
  it changed — the plan picks one (favor: the scan loop re-reads `pane.size()` on wake and resizes the
  `Screen` when it differs, avoiding new cross-task plumbing).
- **No wire-protocol / app change.** `NeedsInput` already flows to the app via the existing
  `AttentionChanged` path.

## Data flow

```
pane output chunk → Screen::feed → grid updated + signals
    signal (BEL/OSC)         → set_attention(NeedsInput)   (immediate, unchanged)
    (each chunk)             → re-arm idle timer (output does NOT change attention state)
idle timer (~500ms) fires    → if !alt_screen && is_blocking_prompt(last_nonempty_line)
                                  → set_attention(NeedsInput)
client input (user answers)  → set_attention(Working)      (existing path, clears NeedsInput)
client Resize                → pane.resize; scan loop re-reads pane.size() → Screen::resize
```

## Error handling

- **No panic on any input:** every grid index is clamped; unknown/partial sequences are no-ops (as today).
- **Lagged broadcast:** unchanged (skip, keep scanning) — the grid may miss a chunk under extreme load;
  the next repaint corrects the visible screen (acceptable for a best-effort attention heuristic).
- **Resize races:** re-allocating clears the grid; a transient wrong size only delays a correct read to
  the next repaint — never a crash.
- **Alt-screen suppression:** guarantees full-screen apps (vim/less) don't trip content-attention.

## Testing

- **`Screen` grid (pure unit):** print + autowrap places glyphs correctly; wide char occupies 2 cells;
  `CR` overwrite; `LF` at bottom scrolls; `CUP`/`EL`/`ED` redraw a prompt; `RI` at top scrolls down;
  alt-screen enter clears + flag set, leave clears + flag cleared; `resize` re-sizes and clamps cursor;
  a truncated/garbage sequence does not panic and yields a sane grid.
- **`last_nonempty_line` / `snapshot`:** correct bottom-most non-blank row; snapshot returns all rows.
- **`is_blocking_prompt` (pure unit):** matches each curated pattern (incl. case variants) and **rejects**
  bare `$`/`%`/`#`/`❯`/`>` shell prompts and ordinary text.
- **Content-attention integration (daemon):** feed `"Continue? (y/n) "` to a hookless agent → after the
  idle tick, `NeedsInput`; feed a bare shell prompt → stays non-`NeedsInput`; after a content-`NeedsInput`,
  a client **input** message clears it → `Working` (existing path); a `(y/n)` written while in alt-screen
  → suppressed; BEL still escalates immediately (regression). Use a short/overridable idle interval in
  tests to avoid wall-clock waits.
- **Existing suites stay green** (`cargo test --workspace --locked`); the two pre-existing daemon timing
  flakes (`attached_client_gets_attention_changed`, exit-under-load) are unrelated.

## Risks

1. **Prompt-pattern false positives/negatives.** Curated list is conservative (unknown prompts simply
   don't fire — no false alarm); the excluded bare-shell-prompt set prevents the idle-shell trap. New
   patterns are additive later. Covered by `is_blocking_prompt` tests.
2. **Emulator incompleteness.** The bounded VT coverage may mis-render exotic TUIs, but attention only
   reads the bottom line at quiescence, and alt-screen apps are suppressed — mis-render there can't raise
   a false prompt. Chars-only keeps it small.
3. **Idle-timer cost.** One `tokio::time::sleep` per hookless agent, re-armed on output — negligible; only
   hookless agents (typically few) run it.
4. **Resize correctness.** Clear-on-resize loses the grid briefly; shells repaint on `SIGWINCH`, so the
   next read is correct. Acceptable for a heuristic.
5. **(d) coupling.** `snapshot()` is the only concession to M9c; it adds no runtime cost and no coupling.

## Decomposition

**One milestone.** Suggested SDD tasks (refined in the plan):
1. `Screen` core — grid alloc, `print` + cursor advance + autowrap + `unicode-width` dep.
2. Control chars + CSI cursor/erase + `IND`/`RI`/`NEL` scroll.
3. Alt-screen toggle + `resize` + `is_alt_screen`/`line`/`last_nonempty_line`/`snapshot`.
4. Fold signal detection into `Screen`'s `Perform` (`feed` returns `AttentionSignal`s), parity with
   `SignalScanner`.
5. `is_blocking_prompt` curated matcher.
6. Daemon integration — swap the hookless scanner to `Screen`, idle-timer `select!`, content-attention +
   resize wiring.
7. Content-attention integration tests.

## Verification gate

A hookless (`shell`) agent that runs a command which blocks at an interactive prompt (`… (y/n)`, a
password prompt, a pager) is flagged **`NeedsInput`** shortly after the output goes quiet, and returns to
`Working` when the user answers (the existing client-input path clears `NeedsInput`) — driven by a
headless `Screen` cell grid in `clowder-vt` that never panics,
tracks the visible screen (glyphs + cursor + alt-screen), and is suppressed inside full-screen apps; a
bare idle shell prompt is **never** flagged. No wire-protocol or macOS-app change. `Screen::snapshot()`
exists for later M9c reuse. Deferred: parsed scrollback/search, SGR fidelity, grids for hook-based agents,
and all M9c survival wiring.
