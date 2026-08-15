//! Insula — Milestone 2: CLI presentation layer.
//!
//! The CLI is a separate process from the detection-engine daemon
//! (`src/main.rs`), launched only when the user accepts the "Launch
//! Insula" action on the OS notification (`ARCHITECTURE.md` Stage 2).

pub mod background;
pub mod command_line;
pub mod kitty_mark;
pub mod notify;
pub mod onboarding;
