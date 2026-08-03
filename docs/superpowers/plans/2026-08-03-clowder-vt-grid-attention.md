# VT grid + content-based attention Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A headless visible-screen cell grid in `clowder-vt` that lets the daemon flag a hookless agent `NeedsInput` when it's blocked at an interactive prompt.

**Architecture:** A new `Screen` type in `clowder-vt` owns a `vte::Parser` + a `rows×cols` char grid (chars only, cursor, alt-screen flag) and a single `vte::Perform` that maintains the grid **and** emits the existing `AttentionSignal`s. A pure `is_blocking_prompt` matcher recognizes interactive prompts. The daemon's hookless scanner swaps `SignalScanner` → `Screen`, adds a quiescence idle-timer, and escalates to `NeedsInput` when a blocking prompt shows at rest (not in alt-screen).

**Tech Stack:** Rust (edition 2021), `vte = "0.13"`, `unicode-width` (new dep), `tokio` (`select!` + `time`).

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` (rustup not auto-sourced here).
- **Edition 2021, stable toolchain.** CI runs `cargo test --workspace --locked` and must stay green.
- **`vte 0.13`**: `Parser::advance` takes **one byte at a time** (`parser.advance(&mut perform, byte)`); `Perform` methods are `print(char)`, `execute(u8)`, `hook(&Params,&[u8],bool,char)`, `put(u8)`, `unhook()`, `osc_dispatch(&[&[u8]],bool)`, `csi_dispatch(&Params,&[u8],bool,char)`, `esc_dispatch(&[u8],bool,u8)`.
- **Chars-only grid** — no SGR color/style. Wide chars via `unicode_width::UnicodeWidthChar::width`; width-0 (combining) glyphs are dropped this milestone.
- **Never panic on any byte input** — clamp every grid index; unknown/partial sequences are safe no-ops.
- **Single parse pass** — one `vte::Perform` does grid + signals; do not run two parsers.
- **Hookless agents only** (`!adapter.provides_hooks()`); **no wire-protocol or macOS-app change**; **no M9c wiring** (only `Screen::snapshot()` exists as a future substrate).
- **Attention state is input-cleared** — content-attention only ever *sets* `NeedsInput`; clearing stays the existing client-input path (`server.rs:732-736`). Never downgrade `Working`/`Completed`/`Exited`.
- Two pre-existing daemon timing tests (`attached_client_gets_attention_changed`, an exit-under-load test) are flaky under load — pass on re-run, NOT regressions.

---

### Task 1: `Screen` scaffolding — grid, `print`, cursor, autowrap

**Files:**
- Modify: `crates/clowder-vt/Cargo.toml` (add `unicode-width`)
- Create: `crates/clowder-vt/src/screen.rs`
- Modify: `crates/clowder-vt/src/lib.rs` (add `mod screen; pub use screen::Screen;`)

**Interfaces:**
- Produces: `pub struct Screen`; `Screen::new(cols: u16, rows: u16) -> Screen`; `Screen::feed(&mut self, bytes: &[u8]) -> Vec<crate::AttentionSignal>` (returns empty until Task 4 folds in signals); `Screen::cursor(&self) -> (u16, u16)` (col, row, 0-based); `Screen::line(&self, row: u16) -> String` (row glyphs, trailing blanks trimmed). Internal `ScreenInner: vte::Perform` with a `put_char`/`scroll_up` and stubbed (no-op) trait methods except `print`.

- [ ] **Step 1: Write the failing tests**

Create `crates/clowder-vt/src/screen.rs` with a `#[cfg(test)] mod tests` containing:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_places_glyphs_and_advances_cursor() {
        let mut s = Screen::new(10, 3);
        assert!(s.feed(b"hi").is_empty());
        assert_eq!(s.line(0), "hi");
        assert_eq!(s.cursor(), (2, 0));
    }

    #[test]
    fn autowrap_moves_to_next_row() {
        let mut s = Screen::new(3, 2);
        s.feed(b"abcd");
        assert_eq!(s.line(0), "abc");
        assert_eq!(s.line(1), "d");
        assert_eq!(s.cursor(), (1, 1));
    }

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut s = Screen::new(4, 1);
        s.feed("世".as_bytes());
        assert_eq!(s.line(0), "世");
        assert_eq!(s.cursor(), (2, 0));
    }

    #[test]
    fn autowrap_on_last_row_scrolls() {
        let mut s = Screen::new(2, 2);
        s.feed(b"aabb");        // fills both rows
        s.feed(b"cc");          // wraps past last row -> scroll up
        assert_eq!(s.line(0), "bb");
        assert_eq!(s.line(1), "cc");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt screen:: 2>&1 | tail -20`
Expected: compile error (`Screen` undefined / module missing).

- [ ] **Step 3: Implement**

Add to `crates/clowder-vt/Cargo.toml` under `[dependencies]` (below `vte = "0.13"`):

```toml
unicode-width = "0.1"
```

Add to the top of `crates/clowder-vt/src/lib.rs` (after the doc comment, before the existing items):

```rust
mod screen;
pub use screen::Screen;
```

Write `crates/clowder-vt/src/screen.rs` (above the test module):

```rust
//! Headless visible-screen cell grid driven by the `vte` parser. Chars-only (no SGR); tracks
//! glyphs + cursor + alternate-screen state — enough to read the bottom-of-screen prompt.

use crate::AttentionSignal;
use unicode_width::UnicodeWidthChar;

/// A parsed visible-screen cell grid. `feed` drives it and returns any attention signals.
pub struct Screen {
    parser: vte::Parser,
    inner: ScreenInner,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { parser: vte::Parser::new(), inner: ScreenInner::new(cols.max(1), rows.max(1)) }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<AttentionSignal> {
        for &b in bytes {
            self.parser.advance(&mut self.inner, b);
        }
        std::mem::take(&mut self.inner.signals)
    }

    pub fn cursor(&self) -> (u16, u16) { (self.inner.cx, self.inner.cy) }

    pub fn line(&self, row: u16) -> String {
        self.inner
            .grid
            .get(row as usize)
            .map(|r| r.iter().collect::<String>().trim_end().to_string())
            .unwrap_or_default()
    }
}

struct ScreenInner {
    cols: u16,
    rows: u16,
    grid: Vec<Vec<char>>, // rows × cols
    cx: u16,
    cy: u16,
    alt: bool,
    signals: Vec<AttentionSignal>,
}

impl ScreenInner {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            grid: vec![vec![' '; cols as usize]; rows as usize],
            cx: 0,
            cy: 0,
            alt: false,
            signals: Vec::new(),
        }
    }

    fn scroll_up(&mut self) {
        self.grid.remove(0);
        self.grid.push(vec![' '; self.cols as usize]);
    }

    fn put_char(&mut self, c: char) {
        let w = UnicodeWidthChar::width(c).unwrap_or(0) as u16;
        if w == 0 {
            return; // drop combining / zero-width this milestone
        }
        if self.cx + w > self.cols {
            self.cx = 0;
            if self.cy + 1 >= self.rows { self.scroll_up(); } else { self.cy += 1; }
        }
        let (x, y) = (self.cx as usize, self.cy as usize);
        if let Some(row) = self.grid.get_mut(y) {
            if x < row.len() { row[x] = c; }
            if w == 2 && x + 1 < row.len() { row[x + 1] = ' '; }
        }
        self.cx += w;
    }
}

impl vte::Perform for ScreenInner {
    fn print(&mut self, c: char) { self.put_char(c); }
    fn execute(&mut self, _byte: u8) {}
    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}
    fn csi_dispatch(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt screen:: 2>&1 | tail -20`
Expected: PASS (4 tests). Also `cargo test -p clowder-vt 2>&1 | tail -5` — existing `SignalScanner` tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-vt/Cargo.toml crates/clowder-vt/src/screen.rs crates/clowder-vt/src/lib.rs Cargo.lock
git commit -m "feat(vt): Screen grid scaffolding — print, cursor, autowrap"
```

---

### Task 2: control chars + CSI cursor/erase + IND/RI/NEL

**Files:**
- Modify: `crates/clowder-vt/src/screen.rs` (`ScreenInner`: `execute`, `csi_dispatch`, `esc_dispatch`, helpers)

**Interfaces:**
- Consumes: Task 1's `ScreenInner` (grid, `cx`/`cy`, `scroll_up`, `cols`/`rows`).
- Produces: no new public API; `execute` handles `LF`/`CR`/`BS`/`HT`, `csi_dispatch` handles cursor `A/B/C/D`, `H`/`f`, `G`, `d` and erase `J`/`K`, `esc_dispatch` handles `IND`(`D`)/`RI`(`M`)/`NEL`(`E`). New helpers `line_feed`, `reverse_index`, `scroll_down`, `erase_line`, `erase_display`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `screen.rs`:

```rust
#[test]
fn cr_returns_to_col0_and_overwrites() {
    let mut s = Screen::new(10, 1);
    s.feed(b"hello\rH");
    assert_eq!(s.line(0), "Hello");
    assert_eq!(s.cursor(), (1, 0));
}

#[test]
fn lf_at_bottom_scrolls() {
    let mut s = Screen::new(4, 2);
    s.feed(b"top\r\nbot\r\nnew");   // third line forces a scroll
    assert_eq!(s.line(0), "bot");
    assert_eq!(s.line(1), "new");
}

#[test]
fn backspace_and_tab_move_cursor() {
    let mut s = Screen::new(20, 1);
    s.feed(b"ab\x08");             // BS
    assert_eq!(s.cursor(), (1, 0));
    let mut s2 = Screen::new(20, 1);
    s2.feed(b"\t");                // HT -> col 8
    assert_eq!(s2.cursor(), (8, 0));
}

#[test]
fn cup_positions_cursor_1_based() {
    let mut s = Screen::new(10, 5);
    s.feed(b"\x1b[3;5H");          // row 3, col 5 (1-based) -> (4,2) 0-based
    assert_eq!(s.cursor(), (4, 2));
}

#[test]
fn erase_line_and_display() {
    let mut s = Screen::new(6, 2);
    s.feed(b"abcdef\r\nghijkl");
    s.feed(b"\x1b[H");             // home (1,1)
    s.feed(b"\x1b[2K");           // erase whole current line (row 0)
    assert_eq!(s.line(0), "");
    assert_eq!(s.line(1), "ghijkl");
    s.feed(b"\x1b[2J");           // erase whole display
    assert_eq!(s.line(1), "");
}

#[test]
fn reverse_index_at_top_scrolls_down() {
    let mut s = Screen::new(4, 2);
    s.feed(b"aa\r\nbb");          // row0="aa", row1="bb", cursor on row1
    s.feed(b"\x1b[H");            // home -> row 0
    s.feed(b"\x1bM");            // RI at top -> scroll down
    assert_eq!(s.line(0), "");
    assert_eq!(s.line(1), "aa");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt screen:: 2>&1 | tail -20`
Expected: FAIL (cursor/erase are no-ops; assertions mismatch).

- [ ] **Step 3: Implement**

Add these helper methods to `impl ScreenInner` (next to `put_char`):

```rust
fn line_feed(&mut self) {
    if self.cy + 1 >= self.rows { self.scroll_up(); } else { self.cy += 1; }
}

fn reverse_index(&mut self) {
    if self.cy == 0 { self.scroll_down(); } else { self.cy -= 1; }
}

fn scroll_down(&mut self) {
    self.grid.pop();
    self.grid.insert(0, vec![' '; self.cols as usize]);
}

fn erase_line(&mut self, mode: u16) {
    let (x, y) = (self.cx as usize, self.cy as usize);
    if let Some(row) = self.grid.get_mut(y) {
        match mode {
            0 => row.iter_mut().skip(x).for_each(|c| *c = ' '),
            1 => row.iter_mut().take(x + 1).for_each(|c| *c = ' '),
            2 => row.iter_mut().for_each(|c| *c = ' '),
            _ => {}
        }
    }
}

fn erase_display(&mut self, mode: u16) {
    match mode {
        0 => {
            self.erase_line(0);
            for r in (self.cy as usize + 1)..self.grid.len() {
                self.grid[r].iter_mut().for_each(|c| *c = ' ');
            }
        }
        1 => {
            for r in 0..(self.cy as usize) {
                self.grid[r].iter_mut().for_each(|c| *c = ' ');
            }
            self.erase_line(1);
        }
        2 | 3 => self.grid.iter_mut().for_each(|row| row.iter_mut().for_each(|c| *c = ' ')),
        _ => {}
    }
}
```

Replace the `execute`, `csi_dispatch`, and `esc_dispatch` no-op stubs in `impl vte::Perform for ScreenInner`:

```rust
fn execute(&mut self, byte: u8) {
    match byte {
        0x0A => self.line_feed(),                       // LF
        0x0D => self.cx = 0,                            // CR
        0x08 => self.cx = self.cx.saturating_sub(1),    // BS
        0x09 => {                                       // HT -> next multiple of 8
            let next = (self.cx / 8 + 1) * 8;
            self.cx = next.min(self.cols.saturating_sub(1));
        }
        _ => {}
    }
}

fn csi_dispatch(&mut self, params: &vte::Params, _intermediates: &[u8], _ignore: bool, action: char) {
    let ps: Vec<u16> = params.iter().map(|s| s.first().copied().unwrap_or(0)).collect();
    // param i with default d applied when the value is 0/absent
    let p = |i: usize, d: u16| {
        let v = ps.get(i).copied().unwrap_or(0);
        if v == 0 { d } else { v }
    };
    let max_x = self.cols.saturating_sub(1);
    let max_y = self.rows.saturating_sub(1);
    match action {
        'A' => self.cy = self.cy.saturating_sub(p(0, 1)),
        'B' => self.cy = (self.cy + p(0, 1)).min(max_y),
        'C' => self.cx = (self.cx + p(0, 1)).min(max_x),
        'D' => self.cx = self.cx.saturating_sub(p(0, 1)),
        'G' => self.cx = p(0, 1).saturating_sub(1).min(max_x),
        'd' => self.cy = p(0, 1).saturating_sub(1).min(max_y),
        'H' | 'f' => {
            self.cy = p(0, 1).saturating_sub(1).min(max_y);
            self.cx = p(1, 1).saturating_sub(1).min(max_x);
        }
        'J' => self.erase_display(ps.first().copied().unwrap_or(0)),
        'K' => self.erase_line(ps.first().copied().unwrap_or(0)),
        _ => {} // SGR ('m') and everything else: no-op (chars-only grid)
    }
}

fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
    match byte {
        b'D' => self.line_feed(),                       // IND
        b'M' => self.reverse_index(),                   // RI
        b'E' => { self.cx = 0; self.line_feed(); }      // NEL
        _ => {}
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt screen:: 2>&1 | tail -20`
Expected: PASS (Task 1 + Task 2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-vt/src/screen.rs
git commit -m "feat(vt): control chars, CSI cursor/erase, IND/RI/NEL"
```

---

### Task 3: alt-screen toggle + resize + query methods

**Files:**
- Modify: `crates/clowder-vt/src/screen.rs` (private-mode branch in `csi_dispatch`; `Screen::resize`/`is_alt_screen`/`last_nonempty_line`/`snapshot`; `ScreenInner::resize`/`set_alt`/`clear`)

**Interfaces:**
- Consumes: Task 2's `ScreenInner`.
- Produces: `Screen::resize(&mut self, cols: u16, rows: u16)`; `Screen::is_alt_screen(&self) -> bool`; `Screen::last_nonempty_line(&self) -> String`; `Screen::snapshot(&self) -> Vec<String>`. `csi_dispatch` toggles `alt` on `?`-private `h`/`l` for `1049`/`1047`/`47` and clears the grid on each switch.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `screen.rs`:

```rust
#[test]
fn alt_screen_enter_clears_and_flags() {
    let mut s = Screen::new(10, 2);
    s.feed(b"normal");
    assert!(!s.is_alt_screen());
    s.feed(b"\x1b[?1049h");
    assert!(s.is_alt_screen());
    assert_eq!(s.line(0), "");                 // prior content hidden
    s.feed(b"\x1b[?1049l");
    assert!(!s.is_alt_screen());
    assert_eq!(s.line(0), "");                 // cleared on leave too
}

#[test]
fn resize_reallocates_and_clamps_cursor() {
    let mut s = Screen::new(10, 4);
    s.feed(b"\x1b[4;9H");                       // cursor near bottom-right
    s.resize(5, 2);
    let (cx, cy) = s.cursor();
    assert!(cx <= 4 && cy <= 1);
    assert_eq!(s.line(0), "");
}

#[test]
fn last_nonempty_line_and_snapshot() {
    let mut s = Screen::new(8, 3);
    s.feed(b"first\r\n\r\n");                    // row0="first", rows 1-2 blank
    assert_eq!(s.last_nonempty_line(), "first");
    assert_eq!(s.snapshot(), vec!["first".to_string(), String::new(), String::new()]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt screen:: 2>&1 | tail -20`
Expected: compile error (`resize`/`is_alt_screen`/`last_nonempty_line`/`snapshot` undefined).

- [ ] **Step 3: Implement**

Add to `impl Screen` (after `line`):

```rust
pub fn resize(&mut self, cols: u16, rows: u16) {
    self.inner.resize(cols.max(1), rows.max(1));
}

pub fn is_alt_screen(&self) -> bool { self.inner.alt }

pub fn last_nonempty_line(&self) -> String {
    for row in self.inner.grid.iter().rev() {
        let t = row.iter().collect::<String>().trim_end().to_string();
        if !t.is_empty() { return t; }
    }
    String::new()
}

pub fn snapshot(&self) -> Vec<String> {
    self.inner
        .grid
        .iter()
        .map(|r| r.iter().collect::<String>().trim_end().to_string())
        .collect()
}
```

Add to `impl ScreenInner` (next to `erase_display`):

```rust
fn clear(&mut self) {
    self.grid.iter_mut().for_each(|row| row.iter_mut().for_each(|c| *c = ' '));
}

fn set_alt(&mut self, on: bool) {
    if on != self.alt {
        self.alt = on;
        self.clear();          // clear on enter AND on leave
        self.cx = 0;
        self.cy = 0;
    }
}

fn resize(&mut self, cols: u16, rows: u16) {
    self.cols = cols;
    self.rows = rows;
    self.grid = vec![vec![' '; cols as usize]; rows as usize];
    self.cx = self.cx.min(cols.saturating_sub(1));
    self.cy = self.cy.min(rows.saturating_sub(1));
}
```

Add a private-mode branch at the **top** of `csi_dispatch` (before the `ps`/`match action` from Task 2). Change the `_intermediates` binding to `intermediates` and insert:

```rust
fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
    let ps: Vec<u16> = params.iter().map(|s| s.first().copied().unwrap_or(0)).collect();
    if intermediates.first() == Some(&b'?') {
        if action == 'h' || action == 'l' {
            let on = action == 'h';
            if ps.iter().any(|c| matches!(c, 1049 | 1047 | 47)) {
                self.set_alt(on);
            }
        }
        return; // private modes are not cursor/erase ops
    }
    let p = |i: usize, d: u16| {
        let v = ps.get(i).copied().unwrap_or(0);
        if v == 0 { d } else { v }
    };
    // ... (the max_x/max_y + match action { ... } body from Task 2 stays unchanged) ...
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt screen:: 2>&1 | tail -20`
Expected: PASS (Tasks 1–3).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-vt/src/screen.rs
git commit -m "feat(vt): alt-screen toggle, resize, snapshot/last_nonempty_line"
```

---

### Task 4: fold signal detection into `Screen`

**Files:**
- Modify: `crates/clowder-vt/src/screen.rs` (`ScreenInner::execute` pushes `Bell`; `osc_dispatch` pushes `Notify`)

**Interfaces:**
- Consumes: `crate::AttentionSignal` (`Bell`, `Notify { title, body }`), the `signals` field, and `feed`'s `std::mem::take` (Task 1).
- Produces: `Screen::feed` now returns the same `AttentionSignal`s `SignalScanner` would — BEL → `Bell`; OSC 9 → `Notify { title: "", body }`; OSC 777 `notify` → `Notify { title, body }` — while still updating the grid.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `screen.rs`:

```rust
#[test]
fn bell_is_a_signal_and_not_printed() {
    let mut s = Screen::new(10, 1);
    assert_eq!(s.feed(b"a\x07b"), vec![AttentionSignal::Bell]);
    assert_eq!(s.line(0), "ab");   // BEL doesn't occupy a cell
}

#[test]
fn osc9_and_osc777_notify() {
    let mut s = Screen::new(10, 1);
    assert_eq!(
        s.feed(b"\x1b]9;hello\x07"),
        vec![AttentionSignal::Notify { title: String::new(), body: "hello".into() }]
    );
    let mut s2 = Screen::new(10, 1);
    assert_eq!(
        s2.feed(b"\x1b]777;notify;Title;Body\x1b\\"),
        vec![AttentionSignal::Notify { title: "Title".into(), body: "Body".into() }]
    );
}

#[test]
fn title_osc_is_ignored_by_screen() {
    let mut s = Screen::new(20, 1);
    assert_eq!(s.feed(b"\x1b]0;my window title\x07"), vec![]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt screen:: 2>&1 | tail -20`
Expected: FAIL (feed returns empty; no signals).

- [ ] **Step 3: Implement**

In `impl vte::Perform for ScreenInner`, add the `0x07` arm to `execute` (keep the other arms from Task 2):

```rust
fn execute(&mut self, byte: u8) {
    match byte {
        0x07 => self.signals.push(AttentionSignal::Bell),   // BEL
        0x0A => self.line_feed(),
        0x0D => self.cx = 0,
        0x08 => self.cx = self.cx.saturating_sub(1),
        0x09 => {
            let next = (self.cx / 8 + 1) * 8;
            self.cx = next.min(self.cols.saturating_sub(1));
        }
        _ => {}
    }
}
```

Replace the `osc_dispatch` no-op with the same logic `SignalScanner`'s `Collector` uses (`crates/clowder-vt/src/lib.rs:48-66`):

```rust
fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
    match params.first() {
        Some(p) if *p == b"9" => {
            let body = params
                .get(1)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            self.signals.push(AttentionSignal::Notify { title: String::new(), body });
        }
        Some(p) if *p == b"777" => {
            if params.get(1).map(|b| *b == b"notify").unwrap_or(false) {
                let title = params.get(2).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
                let body = params.get(3).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
                self.signals.push(AttentionSignal::Notify { title, body });
            }
        }
        _ => {}
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt 2>&1 | tail -20`
Expected: PASS (all `clowder-vt` tests — `Screen` signal parity + existing `SignalScanner` tests).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-vt/src/screen.rs
git commit -m "feat(vt): Screen emits BEL/OSC attention signals (SignalScanner parity)"
```

---

### Task 5: `is_blocking_prompt` curated matcher

**Files:**
- Create: `crates/clowder-vt/src/prompt.rs`
- Modify: `crates/clowder-vt/src/lib.rs` (`mod prompt; pub use prompt::is_blocking_prompt;`)

**Interfaces:**
- Produces: `pub fn is_blocking_prompt(line: &str) -> bool` — true for a curated set of interactive prompts, false for bare shell prompts and ordinary text.

- [ ] **Step 1: Write the failing tests**

Create `crates/clowder-vt/src/prompt.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_interactive_prompts() {
        for line in [
            "Continue? (y/n)",
            "Overwrite? [Y/n] ",
            "Proceed (yes/no)?",
            "Delete everything? [y/N]:",
            "Password:",
            "Enter passphrase for key '/x':",
            "[sudo] password for alice:",
            "Press ENTER to continue",
            "Press any key to continue . . .",
            "--More--",
            "lines 1-10 (END)",
            "? Select an option",
            ">>> ",
            "In [12]:",
        ] {
            assert!(is_blocking_prompt(line), "should match: {line:?}");
        }
    }

    #[test]
    fn rejects_shell_prompts_and_text() {
        for line in [
            "$ ",
            "user@host:~/proj$ ",
            "% ",
            "# ",
            "❯ ",
            "> ",
            "building project...",
            "error: something failed",
            "",
        ] {
            assert!(!is_blocking_prompt(line), "should NOT match: {line:?}");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt prompt:: 2>&1 | tail -20`
Expected: compile error (`is_blocking_prompt` undefined).

- [ ] **Step 3: Implement**

Add to `crates/clowder-vt/src/lib.rs` (next to the `mod screen;` line):

```rust
mod prompt;
pub use prompt::is_blocking_prompt;
```

Write `crates/clowder-vt/src/prompt.rs` (above the test module):

```rust
//! Curated matcher: does a rendered line look like a program blocking on interactive input?
//! Conservative — unknown prompts simply don't match (no false alarm), and bare shell prompts
//! are deliberately excluded so an idle shell never reads as "needs input".

/// True when `line` (a rendered screen line) looks like a blocking interactive prompt.
pub fn is_blocking_prompt(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();

    // yes/no prompts, optionally followed by ? or :
    const YN: &[&str] = &["(y/n)", "(yes/no)", "[y/n]", "[yes/no]"];
    if YN.iter().any(|p| {
        lower.ends_with(p)
            || lower.ends_with(&format!("{p}?"))
            || lower.ends_with(&format!("{p}:"))
    }) {
        return true;
    }

    // password / passphrase
    if lower.ends_with("password:")
        || lower.ends_with("passphrase:")
        || (lower.contains("password for") && lower.ends_with(':'))
        || (lower.contains("passphrase for") && lower.ends_with(':'))
    {
        return true;
    }

    // press <key> to continue
    if lower.contains("press enter") || lower.contains("press return") || lower.contains("press any key") {
        return true;
    }

    // pagers
    if t.contains("--More--") || lower.ends_with("(end)") {
        return true;
    }

    // inquirer-style question
    if t.starts_with("? ") {
        return true;
    }

    // REPLs
    if lower.ends_with(">>>") {
        return true;
    }
    if lower.starts_with("in [") && lower.ends_with("]:") {
        return true;
    }

    false
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-vt prompt:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-vt/src/prompt.rs crates/clowder-vt/src/lib.rs
git commit -m "feat(vt): is_blocking_prompt curated interactive-prompt matcher"
```

---

### Task 6: daemon integration — Screen scanner + quiescence idle-timer

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (`Daemon` field `content_idle`; rewrite the hookless scanner block in `finalize_agent`, `server.rs:281-310`)

**Interfaces:**
- Consumes: `clowder_vt::Screen` (`new`/`feed`/`resize`/`is_alt_screen`/`last_nonempty_line`), `clowder_vt::is_blocking_prompt`, `Pane::size()`, `Pane::snapshot_and_subscribe()`.
- Produces: `Daemon.content_idle: std::time::Duration` (`pub(crate)`, default 500 ms; tests may shrink it before `Arc`-wrapping). Hookless agents get a `Screen`-backed scanner that escalates on BEL/OSC immediately and, at quiescence, on a blocking prompt (not in alt-screen).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `server.rs` (this drives the new wiring end-to-end; see the shared helper below in Task 7 — for Task 6, inline a minimal version):

```rust
#[tokio::test]
async fn hookless_prompt_sets_needs_input_after_idle() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use std::process::Command as PCommand;
    use std::time::Duration;

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let statedir = tempfile::tempdir().unwrap();
    let _lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));

    let mut d = Daemon::new_with(Arc::new(FakeNotifier::new()), "/tmp/unused-vt1.sock".into());
    d.content_idle = Duration::from_millis(40);
    let daemon = Arc::new(d);
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'Continue? (y/n) '; sleep 30".into()],
            cwd: None, env: vec![],
        },
    };
    let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();

    // Poll up to ~3s for the content-attention escalation.
    let mut got = false;
    for _ in 0..150 {
        if daemon.attention_of(id) == Some(AttentionState::NeedsInput) { got = true; break; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(got, "blocking prompt should escalate to NeedsInput");

    daemon.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon hookless_prompt_sets_needs_input_after_idle 2>&1 | tail -20`
Expected: compile error (`content_idle` field missing), then — once the field is added but the scanner not rewritten — the test would fail (no escalation). Implement fully in Step 3.

- [ ] **Step 3: Implement**

Add the field to `struct Daemon` (after the `layout_dirty` field):

```rust
    layout_dirty: Arc<Mutex<std::collections::HashSet<PaneId>>>,
    /// Idle debounce before content-based attention inspects the screen for a blocking prompt.
    pub(crate) content_idle: std::time::Duration,
```

Initialize it in `new_with`'s struct literal (after the `layout_dirty: ...` line):

```rust
            layout_dirty: Arc::new(Mutex::new(std::collections::HashSet::new())),
            content_idle: std::time::Duration::from_millis(500),
```

Replace the hookless scanner block (`server.rs:281-310`, the `if !adapter.provides_hooks() { ... }`) with:

```rust
if !adapter.provides_hooks() {
    self.hookless.lock().insert(id);
    if let Some(pane_arc) = self.panes.lock().get(&id).cloned() {
        let me = Arc::clone(self);
        let idle = self.content_idle;
        let far = std::time::Duration::from_secs(3600);
        let (snapshot, mut rx) = pane_arc.snapshot_and_subscribe();
        let handle = tokio::spawn(async move {
            let (mut cols, mut rows) = pane_arc.size();
            let mut screen = clowder_vt::Screen::new(cols, rows);
            // Output produced before we subscribed (no lost early signal).
            if !screen.feed(&snapshot).is_empty()
                && me.attention_of(id) != Some(AttentionState::NeedsInput)
            {
                me.set_attention(id, AttentionState::NeedsInput);
            }
            let timer = tokio::time::sleep(idle);
            tokio::pin!(timer);
            loop {
                tokio::select! {
                    r = rx.recv() => match r {
                        Ok(chunk) => {
                            // BEL/OSC escalate immediately (unchanged behavior).
                            if !screen.feed(&chunk).is_empty()
                                && me.attention_of(id) != Some(AttentionState::NeedsInput)
                            {
                                me.set_attention(id, AttentionState::NeedsInput);
                            }
                            let (nc, nr) = pane_arc.size();
                            if (nc, nr) != (cols, rows) {
                                cols = nc; rows = nr;
                                screen.resize(cols, rows);
                            }
                            // New output re-arms the quiescence timer.
                            timer.as_mut().reset(tokio::time::Instant::now() + idle);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break, // pane gone
                    },
                    _ = &mut timer => {
                        // Quiescent: a blocking prompt at rest (not in a full-screen app) → NeedsInput.
                        if !screen.is_alt_screen()
                            && clowder_vt::is_blocking_prompt(&screen.last_nonempty_line())
                            && me.attention_of(id) != Some(AttentionState::NeedsInput)
                        {
                            me.set_attention(id, AttentionState::NeedsInput);
                        }
                        // Park until the next output re-arms it (avoid busy-spin on an elapsed sleep).
                        timer.as_mut().reset(tokio::time::Instant::now() + far);
                    }
                }
            }
        });
        self.scanners.lock().insert(id, handle);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon hookless_prompt_sets_needs_input_after_idle 2>&1 | tail -20`
Then the whole crate: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon 2>&1 | tail -20` (re-run once if `attached_client_gets_attention_changed` flakes — known timing flake).
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "feat(daemon): content-based attention via Screen + quiescence idle-timer"
```

---

### Task 7: content-attention integration tests

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (`#[cfg(test)] mod tests`) — a shared helper + three more scenarios; no production changes.

**Interfaces:**
- Consumes: everything above (`spawn_agent`, `attention_of`, `content_idle`, `SyntheticAdapter`).
- Produces: tests for bare-shell-prompt (no escalation), alt-screen suppression, and immediate BEL escalation.

- [ ] **Step 1: Write the tests**

Add to the `tests` module in `server.rs` a shared helper and three tests:

```rust
/// Spawn a hookless agent running `script` under /bin/sh with a short content-idle. Returns the
/// daemon, the agent id, and guards (tempdirs + the env lock) the caller must keep alive.
async fn spawn_hookless(
    script: &str,
) -> (Arc<Daemon>, PaneId, tempfile::TempDir, tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    use crate::{FakeNotifier, SyntheticAdapter};
    use std::process::Command as PCommand;
    use std::time::Duration;

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let statedir = tempfile::tempdir().unwrap();
    let lock = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));

    let mut d = Daemon::new_with(Arc::new(FakeNotifier::new()), "/tmp/unused-vt.sock".into());
    d.content_idle = Duration::from_millis(40);
    let daemon = Arc::new(d);
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None, env: vec![],
        },
    };
    let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();
    (daemon, id, repo, statedir, lock)
}

async fn wait_for(daemon: &Daemon, id: PaneId, want: AttentionState, ticks: u32) -> bool {
    for _ in 0..ticks {
        if daemon.attention_of(id) == Some(want) { return true; }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    daemon.attention_of(id) == Some(want)
}

#[tokio::test]
async fn bare_shell_prompt_does_not_escalate() {
    let (daemon, id, _r, _s, _lock) = spawn_hookless("printf '$ '; sleep 30").await;
    // Give the idle timer several windows to (not) fire.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_ne!(daemon.attention_of(id), Some(AttentionState::NeedsInput),
        "a bare shell prompt must not read as NeedsInput");
    daemon.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}

#[tokio::test]
async fn alt_screen_prompt_is_suppressed() {
    // Enter alt-screen, then draw a (y/n): content-attention must be suppressed.
    let (daemon, id, _r, _s, _lock) =
        spawn_hookless("printf '\\033[?1049hContinue? (y/n) '; sleep 30").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_ne!(daemon.attention_of(id), Some(AttentionState::NeedsInput),
        "a prompt inside the alternate screen must be suppressed");
    daemon.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}

#[tokio::test]
async fn bell_still_escalates_immediately() {
    let (daemon, id, _r, _s, _lock) = spawn_hookless("printf '\\007'; sleep 30").await;
    assert!(wait_for(&daemon, id, AttentionState::NeedsInput, 150).await,
        "BEL must still escalate to NeedsInput");
    daemon.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}
```

- [ ] **Step 2: Run the tests**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon -- bare_shell_prompt_does_not_escalate alt_screen_prompt_is_suppressed bell_still_escalates_immediately 2>&1 | tail -30`
Expected: PASS. (These exercise the Task 6 wiring; if `bare_shell_prompt_does_not_escalate` fails, `is_blocking_prompt` is matching a bare prompt; if `alt_screen_prompt_is_suppressed` fails, the `is_alt_screen()` guard is missing.)

- [ ] **Step 3: Full-suite check**

Run: `source "$HOME/.cargo/env" && cargo test --workspace --locked 2>&1 | tail -30`
Expected: green (re-run once if `attached_client_gets_attention_changed` / the exit-under-load test flakes — the two known pre-existing flakes).

- [ ] **Step 4: Clippy**

Run: `source "$HOME/.cargo/env" && cargo clippy -p clowder-vt -p clowder-daemon --all-targets 2>&1 | grep -E "warning:|error" | grep -iE "screen|prompt|server" | head`
Expected: no new warnings in `screen.rs` / `prompt.rs` / the changed `server.rs` block.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "test(daemon): content-attention integration — shell prompt, alt-screen, BEL"
```

---

## Notes for the implementer

- **`vte 0.13` one-byte `advance`** — `parser.advance(&mut inner, byte)` per byte; the `Screen::feed` loop already does this. `parser` and `inner` are disjoint struct fields, so `self.parser.advance(&mut self.inner, b)` borrow-checks.
- **Never panic** — every grid write goes through a bounds check (`get_mut`, `.min(...)`, `saturating_sub`); `new`/`resize` clamp `cols`/`rows` to ≥ 1 so `cols-1`/`rows-1` never underflow (`saturating_sub` used regardless).
- **Tests set the process-global `CLOWDER_STATE_FILE`** — always hold `crate::STATE_FILE_ENV_LOCK` (added in M9b) across the env-var span and `remove_var` at the end; each test uses its own tempdir.
- **Timing tests** use `content_idle = 40 ms` and poll with a generous (~3 s) timeout, matching the existing attention tests' style. Real subprocess + timer tests can flake under heavy parallel load — re-run once before treating a failure as real.
- **`unicode-width`** is a new dependency on `clowder-vt` only; commit the regenerated `Cargo.lock` (CI runs `--locked`).
