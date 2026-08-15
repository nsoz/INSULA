//! Insula — Milestone 2: the small status ticker shown between onboarding
//! and the report streaming into `command_line`'s history. Modeled after a
//! CLI agent's own tool-call status line: a short, animated "doing X" line,
//! with the messages that already finished left behind above it, dimmed —
//! not the full scrolling terminal-flow `command_line` owns. Fixed-size,
//! same reasoning as `onboarding` — these lines don't belong in session
//! scrollback, so they don't shrink the watermark either.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::cli::background::BG_COLOR;

pub const HEIGHT: u16 = 9;

const SPINNER_FRAMES: &[char] = &[
    '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280f}',
];

pub struct Notifications<'a> {
    pub past: &'a [String],
    pub current: Option<&'a str>,
    pub spinner_tick: usize,
}

impl Notifications<'_> {
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = self
            .past
            .iter()
            .map(|line| Line::from(format!("  {line}")).style(Style::default().fg(Color::DarkGray)))
            .collect();

        if let Some(current) = self.current {
            let spinner = SPINNER_FRAMES[self.spinner_tick % SPINNER_FRAMES.len()];
            lines.push(
                Line::from(format!("{spinner} {current}")).style(Style::default().fg(Color::White)),
            );
        }

        let paragraph = Paragraph::new(lines).style(Style::default().bg(BG_COLOR));
        frame.render_widget(paragraph, area);
    }
}
