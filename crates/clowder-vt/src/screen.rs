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
