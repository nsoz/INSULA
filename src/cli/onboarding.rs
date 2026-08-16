//! Insula — Milestone 2: the onboarding flow a session starts with.
//!
//! Three questions — welcome, the app path, and the exploration mode —
//! render into a *fixed-size* reserved area, not the growing terminal-flow
//! `command_line` uses for everything after. None of these three answers
//! become part of that scrollback (there's no reason to scroll back to
//! "what path did I type"), so there's no reason for them to shrink the
//! watermark the way real session history does — see `insula_cli::run` for
//! where the two areas actually diverge.

use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::cli::background::BG_COLOR;

/// Rows reserved for onboarding, regardless of which step is showing or how
/// much has been typed into the path field — a constant, not
/// `command_line`'s `needed_height`, precisely so the watermark never
/// shrinks during this phase (see module docs).
pub const HEIGHT: u16 = 9;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExplorationMode {
    Manual,
    Unattended,
}

enum Step {
    Welcome,
    /// `error` holds the reason the last submitted path was rejected (see
    /// `app_target::validate`) — cleared as soon as the user edits the
    /// input again, so it never lingers past the text that caused it.
    Path {
        error: Option<String>,
    },
    Mode {
        selected: ExplorationMode,
    },
}

pub struct Onboarding {
    step: Step,
    input: String,
}

pub struct Answers {
    pub path: String,
    pub mode: ExplorationMode,
}

impl Default for Onboarding {
    fn default() -> Self {
        Self {
            step: Step::Welcome,
            input: String::new(),
        }
    }
}

impl Onboarding {
    /// Returns `Some(answers)` once Enter is pressed on the final step;
    /// `None` while still mid-flow.
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<Answers> {
        match &mut self.step {
            Step::Welcome => {
                if key.code == KeyCode::Enter {
                    self.step = Step::Path { error: None };
                }
            }
            Step::Path { error } => match key.code {
                KeyCode::Char(c) => {
                    self.input.push(c);
                    *error = None;
                }
                KeyCode::Backspace => {
                    self.input.pop();
                    *error = None;
                }
                KeyCode::Enter if !self.input.trim().is_empty() => {
                    let expanded = crate::app_target::expand(&self.input);
                    match crate::app_target::validate(&expanded) {
                        Ok(()) => {
                            self.input = expanded.to_string_lossy().into_owned();
                            self.step = Step::Mode {
                                selected: ExplorationMode::Manual,
                            };
                        }
                        Err(reason) => *error = Some(reason.message().to_string()),
                    }
                }
                _ => {}
            },
            Step::Mode { selected } => match key.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                    *selected = match selected {
                        ExplorationMode::Manual => ExplorationMode::Unattended,
                        ExplorationMode::Unattended => ExplorationMode::Manual,
                    };
                }
                KeyCode::Char('1') => *selected = ExplorationMode::Manual,
                KeyCode::Char('2') => *selected = ExplorationMode::Unattended,
                KeyCode::Enter => {
                    return Some(Answers {
                        path: std::mem::take(&mut self.input),
                        mode: *selected,
                    });
                }
                _ => {}
            },
        }
        None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let lines: Vec<Line> = match &self.step {
            Step::Welcome => vec![
                Line::from(Span::styled(
                    "Welcome to Insula.",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "I'll run the app you want to examine inside an isolated VM",
                    Style::default().fg(Color::Gray),
                )),
                Line::from(Span::styled(
                    "and observe what it does for you.",
                    Style::default().fg(Color::Gray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press Enter to continue.",
                    Style::default().fg(Color::DarkGray),
                )),
            ],
            Step::Path { error } => {
                let mut lines = vec![
                    Line::from(Span::styled(
                        "What's the path to the app you want to run?",
                        Style::default().fg(Color::White),
                    )),
                    Line::from(""),
                    Line::from(format!("> {}", self.input)),
                ];
                if let Some(message) = error {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        message.as_str(),
                        Style::default().fg(Color::Red),
                    )));
                }
                lines
            }
            Step::Mode { selected } => {
                let option = |mode: ExplorationMode, text: &str| {
                    let is_selected = *selected == mode;
                    let marker = if is_selected { "\u{203a}" } else { " " };
                    let style = if is_selected {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Line::from(Span::styled(format!("{marker} {text}"), style))
                };
                vec![
                    Line::from(Span::styled(
                        "How should the exploration happen?",
                        Style::default().fg(Color::White),
                    )),
                    Line::from(""),
                    option(
                        ExplorationMode::Manual,
                        "[1] Manual \u{2014} you use the app yourself inside the VM",
                    ),
                    option(
                        ExplorationMode::Unattended,
                        "[2] Unattended \u{2014} run it in the background and observe, no VM window",
                    ),
                    Line::from(""),
                    Line::from(Span::styled(
                        "\u{2191}/\u{2193} or 1/2 to select, Enter to confirm.",
                        Style::default().fg(Color::DarkGray),
                    )),
                ]
            }
        };

        let paragraph = Paragraph::new(lines)
            .style(Style::default().bg(BG_COLOR))
            .alignment(Alignment::Left);
        frame.render_widget(paragraph, area);

        if matches!(self.step, Step::Path { .. }) {
            let cursor_x = area.x + 2 + self.input.chars().count() as u16;
            let cursor_y = area.y + 2;
            frame.set_cursor_position((cursor_x.min(area.right().saturating_sub(1)), cursor_y));
        }
    }
}
