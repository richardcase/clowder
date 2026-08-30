// SPDX-License-Identifier: Apache-2.0

//! Headless terminal parsing for the daemon, over the `vte` escape-sequence parser:
//! - [`Screen`] — a visible-screen cell grid (glyphs + cursor + alt-screen) that also emits attention
//!   signals (BEL, OSC 9, OSC 777) in a single parse pass, for content-based attention.
//! - [`is_blocking_prompt`] — recognizes an interactive prompt from a rendered line.

mod screen;
pub use screen::Screen;
mod prompt;
pub use prompt::is_blocking_prompt;

/// An attention-worthy signal found in a pane's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttentionSignal {
    Bell,
    Notify { title: String, body: String },
}
