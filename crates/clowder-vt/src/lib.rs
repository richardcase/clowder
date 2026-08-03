//! Headless scanner for terminal attention signals (BEL, OSC 9, OSC 777) using the `vte`
//! escape-sequence parser. No cell grid — just signal detection.

mod screen;
pub use screen::Screen;

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
        // vte 0.13's `Parser::advance` takes one byte at a time (no slice-based overload),
        // unlike the plausible `advance(&mut collector, bytes)` API sketched in the brief.
        for &byte in bytes {
            self.parser.advance(&mut collector, byte);
        }
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
