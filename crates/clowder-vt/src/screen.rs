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
}

impl vte::Perform for ScreenInner {
    fn print(&mut self, c: char) { self.put_char(c); }
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
    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}
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
}

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
}
