//! A classic, borderless terminal-flow command input rendered at the
//! bottom of the CLI: previously entered lines scroll upward exactly like
//! a real shell's scrollback, and the line currently being typed is always
//! the last line — no separate boxed "input widget". The region it
//! occupies grows with how much has actually been typed (see
//! `needed_height`) instead of being a fixed size, the same way a real
//! terminal doesn't reserve a big blank prompt area up front. `Up`/`Down`
//! (and `PageUp`/`PageDown`, and the mouse wheel) scroll back through
//! history once it's grown past what's visible; typing or submitting
//! snaps back to the live bottom.
//!
//! Deliberately rendered into its own reserved region, carved out of the
//! frame *before* the watermark computes its own placement area (see
//! `insula_cli::run`), rather than drawn "on top of" the watermark
//! afterward. For a real graphics-protocol mark, a cell inside its
//! placement doesn't hold a normal character — it can hold the entire
//! escape-sequence payload, or a `Skip` diff marker so Ratatui leaves the
//! terminal's own drawing of it alone. Overwriting a cell like that with
//! normal text only changes its symbol, not that leftover diff marker (see
//! `ratatui_core::buffer::Buffer::set_stringn`), so a widget drawn on top
//! could silently never actually reach the terminal. Reserving a region
//! the watermark never touches sidesteps that entirely.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::cli::background::BG_COLOR;

/// How many previously entered lines are kept, oldest dropped first once
/// full — otherwise this would grow without bound over a long session.
const HISTORY_CAPACITY: usize = 200;

/// How many lines `PageUp`/`PageDown` jump by, vs. one line at a time for
/// `Up`/`Down` — an approximation of "a page", not tied to the actual
/// visible height (which `handle_key` doesn't know; `render` clamps
/// whatever this produces to what's actually visible anyway).
const PAGE_SCROLL: usize = 10;

/// How many lines one mouse wheel "notch" moves — more than a single
/// arrow-key press, matching how a wheel tick scrolls more than one line
/// in most terminal apps.
const WHEEL_SCROLL: usize = 3;

#[derive(Default)]
pub struct CommandLine {
    input: String,
    history: VecDeque<String>,
    /// How many lines scrolled up from the bottom-anchored (latest) view.
    /// `render` clamps this to what's actually scrollable each frame, so
    /// it's safe for this to grow past that between renders.
    scroll: usize,
}

impl CommandLine {
    /// Handles a key event; returns `true` if it was consumed here (the
    /// caller should still handle its own quit key itself, `Esc` is not
    /// treated as input text).
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => self.scroll_by(1),
            KeyCode::Down => self.scroll_by(-1),
            KeyCode::PageUp => self.scroll_by(PAGE_SCROLL as isize),
            KeyCode::PageDown => self.scroll_by(-(PAGE_SCROLL as isize)),
            KeyCode::Char(c) => {
                self.scroll = 0;
                self.input.push(c);
            }
            KeyCode::Backspace => {
                self.scroll = 0;
                self.input.pop();
            }
            KeyCode::Enter => {
                if !self.input.is_empty() {
                    let line = std::mem::take(&mut self.input);
                    self.push_history_line(line);
                }
                // Typing or submitting always snaps back to the live
                // bottom — same as a real terminal jumping to the newest
                // output the moment you interact with the prompt again.
                self.scroll = 0;
            }
            _ => return false,
        }
        true
    }

    /// Appends one character to the in-progress line on the live bottom
    /// row — the same row the user's own typing shows on, but driven by
    /// generated content instead. Used to type-writer-stream a report
    /// character by character; call `commit_streaming_line` once that
    /// line's characters are exhausted to move it into permanent history.
    pub fn push_streaming_char(&mut self, c: char) {
        self.scroll = 0;
        self.input.push(c);
    }

    /// Moves the in-progress streamed line (built via `push_streaming_char`,
    /// possibly empty for a blank spacer line) into permanent history, the
    /// same transition Enter makes for typed input.
    pub fn commit_streaming_line(&mut self) {
        let line = std::mem::take(&mut self.input);
        self.push_history_line(line);
    }

    /// Appends a line directly into history — used to stream generated
    /// content (e.g. the final report) rather than something the user
    /// typed. Snaps scroll back to the live bottom, same as pressing Enter,
    /// so streamed lines stay visible as they arrive.
    pub fn push_history_line(&mut self, line: String) {
        self.scroll = 0;
        if self.history.len() == HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(line);
    }

    /// Handles a mouse event; returns `true` if it was consumed here. Only
    /// the wheel scrolls history — clicks/drags aren't used by anything
    /// yet.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_by(WHEEL_SCROLL as isize),
            MouseEventKind::ScrollDown => self.scroll_by(-(WHEEL_SCROLL as isize)),
            _ => return false,
        }
        true
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
    }

    /// How many rows this wants right now: one per history line still
    /// worth showing, plus the current input line — growing as more gets
    /// typed, the way a real terminal's scrollback claims more of the
    /// window the longer a session runs. Capped at `max` so the watermark
    /// always keeps at least some room.
    pub fn needed_height(&self, max: u16) -> u16 {
        let content = self.history.len() as u16 + 1;
        content.min(max.max(1))
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // The input line is always pinned to the bottom row, live and
        // editable regardless of scroll — only the history above it is a
        // scrollable window, the same split a chat/log TUI uses so you can
        // read back without losing your place in what you're typing.
        let visible_rows = usize::from(area.height.saturating_sub(1));
        let total = self.history.len();
        let max_scroll = total.saturating_sub(visible_rows);
        let scroll = self.scroll.min(max_scroll);
        let end = total - scroll;
        let start = end.saturating_sub(visible_rows);

        let mut lines: Vec<Line> = self
            .history
            .iter()
            .skip(start)
            .take(end - start)
            .map(|line| Line::from(format!("> {line}")).style(Style::default().fg(Color::Gray)))
            .collect();
        lines
            .push(Line::from(format!("> {}", self.input)).style(Style::default().fg(Color::White)));

        let flow = Paragraph::new(lines).style(Style::default().bg(BG_COLOR));
        frame.render_widget(flow, area);

        let cursor_x = area.x + 2 + self.input.chars().count() as u16;
        let cursor_y = area.bottom().saturating_sub(1);
        frame.set_cursor_position((cursor_x.min(area.right().saturating_sub(1)), cursor_y));
    }
}
