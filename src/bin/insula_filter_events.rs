//! Insula — standalone CLI wrapper around `event_filter::filter_for_target`.
//!
//! Not wired into `insula_cli`'s own flow yet (Milestones 5-6 aren't
//! built) — this exists so a captured `events.jsonl` (see `RunningVm::logs_dir`,
//! `src/vm.rs`) can actually be inspected today, for the analysis-noise
//! problem raw sensor output has in practice: a near-idle guest still
//! produces a large volume of background-daemon activity alongside
//! whatever the submitted app itself did.
//!
//! Usage:
//! ```text
//! cargo run --bin insula_filter_events -- <events.jsonl> <submitted-app-path>
//! ```
//! `<submitted-app-path>` is the same path originally handed to
//! `insula_cli` (a `.app` bundle or bare executable) — this resolves it to
//! the guest-side exec path itself via `vm::expected_guest_exec_path`,
//! rather than making the caller work out and type that path by hand.

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(log_path), Some(app_path)) = (args.next(), args.next()) else {
        eprintln!("usage: insula_filter_events <events.jsonl> <submitted-app-path>");
        std::process::exit(2);
    };

    let app_path = PathBuf::from(app_path);
    let Some(target_exec_path) = insula::vm::expected_guest_exec_path(&app_path) else {
        eprintln!(
            "'{}' doesn't look like a runnable app — same check insula_cli's \
             onboarding already ran on it at submission time.",
            app_path.display()
        );
        std::process::exit(1);
    };
    let target_exec_path = target_exec_path.to_string_lossy();

    let raw = match std::fs::read_to_string(&log_path) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("failed to read {log_path}: {e}");
            std::process::exit(1);
        }
    };

    let matched = insula::event_filter::filter_for_target(&raw, &target_exec_path);
    if matched.is_empty() {
        eprintln!(
            "no exec event matching '{target_exec_path}' found in {log_path} — \
             either the app wasn't launched during this run, or its exec path \
             looks different than expected."
        );
        std::process::exit(1);
    }

    for event in matched {
        println!("{event}");
    }
}
