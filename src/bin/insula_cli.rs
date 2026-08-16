//! Insula — Milestone 2: CLI entry point.
//!
//! Launched when the user accepts the "Launch Insula" action on the OS
//! notification (`ARCHITECTURE.md` Stage 2). Renders the persistent
//! background watermark (`insula::cli::background`/`kitty_mark`) with a
//! shell-like command input (`insula::cli::command_line`) reserved at the
//! bottom of the frame — real status/panel content beyond that command
//! line comes later.
//!
//! Renders at full fidelity, with a real-time-resizable mark, in Kitty —
//! see `insula::cli::kitty_mark` for why that's Kitty-specific. Rather than
//! depending on whichever terminal happens to already be focused when this
//! is launched (irrelevant for the real Stage-2 launch path, which starts
//! from an OS notification action, not an existing terminal session), this
//! relaunches itself inside Kitty whenever Kitty is installed but isn't
//! already the current terminal — see `relaunch_in_kitty_if_available`.
//! Terminals without any graphics protocol (e.g. classic Terminal.app) and
//! machines without Kitty installed both still work, falling back to a
//! half-block approximation, which is that terminal's real ceiling, not a
//! bug here — this app never installs anything on its own.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use insula::cli::background::Background;
use insula::cli::command_line::CommandLine;
use insula::cli::kitty_mark::KittyMark;
use insula::cli::notify::Notifications;
use insula::cli::onboarding::{Answers, ExplorationMode, Onboarding};
use insula::vm::RunningVm;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::{Frame, Terminal};
use ratatui_image::picker::{Picker, ProtocolType};

/// Name of the golden, `insula_setup`-prepared image (SIP already
/// disabled, the ESF sensor already installed — see
/// `project-insula-vm-tooling` memory) that every real analysis clones a
/// fresh, disposable instance from. Never run directly — see
/// `vm::RunningVm::launch`.
const GOLDEN_VM_NAME: &str = "insula-macos";

/// How long each notification line stays as the animated "current" one
/// before the next one takes over.
const NOTIFY_INTERVAL: Duration = Duration::from_millis(1100);

/// How long each character waits before the next one is typed into the
/// report — the typewriter effect. Kept well under `ANIMATION_POLL`, since
/// this is what actually controls the perceived speed.
const CHAR_INTERVAL: Duration = Duration::from_millis(3);

/// `event::poll`'s timeout while `Notifying`/`Streaming` are animating —
/// short, so the main loop comes back around often enough for
/// `CHAR_INTERVAL`/`NOTIFY_INTERVAL` to actually bite instead of being
/// capped at one tick per poll wakeup. `IDLE_POLL` is used everywhere else,
/// so onboarding and plain interactive use don't spin the CPU for no
/// reason.
const ANIMATION_POLL: Duration = Duration::from_millis(8);
const IDLE_POLL: Duration = Duration::from_millis(200);

/// How often `Stage::AutoRunning` re-reads and re-filters `events.jsonl`
/// while waiting on a headless, `ExplorationMode::Unattended` run — far
/// coarser than `ANIMATION_POLL` since there's no user-facing animation
/// riding on it, just disk I/O that doesn't need to happen every 8ms.
const AUTO_RUN_POLL: Duration = Duration::from_millis(500);

/// If the target never even starts executing within this long, the
/// guest-side auto-run daemon likely failed (or the resolved exec path
/// was wrong) — give up rather than waiting forever.
///
/// **Live-tested and found too short at 30s** (2026-08-16): a real run
/// timed out with the sensor showing only ~17s of captured boot noise —
/// the target never got a chance to start. The guest side has its own
/// chained worst-case latency this has to outlast: the VM's own boot
/// (`vm.rs`'s doc comment already notes "tens of seconds"), *then*
/// desktop-sync's own up-to-30s wait for its share to mount plus however
/// long the actual copy takes (the `EncryptApp` binaries tested this
/// session were ~80MB, self-contained .NET), *then* auto-run's own two
/// sequential wait loops (marker, then target file) on top of that.
/// 180s gives real slack across that whole chain instead of a number
/// picked before any of it was measured.
const AUTO_RUN_START_TIMEOUT: Duration = Duration::from_secs(180);

/// If the target started but produces no new sensor events (process or
/// file) for this long without exiting, treat it as stuck — an infinite
/// loop, a wait on input that will never come, or similar — rather than
/// waiting forever. Deliberately based on *sensor activity*, not wall
/// clock since launch, so a slow-but-genuinely-working run isn't
/// mistaken for a hang.
const AUTO_RUN_HANG_TIMEOUT: Duration = Duration::from_secs(45);

/// Dispatches to whichever watermark implementation actually gets a smooth
/// live resize out of the current terminal. `KittyMark` transmits the mark
/// once and only ever sends cheap placement updates after that — real-time,
/// but only understood by Kitty-protocol-native terminals (Kitty, Ghostty,
/// WezTerm). Everything else falls back to `Background`, which works
/// anywhere `ratatui_image` has a protocol for, at the cost of a brief
/// re-encode pause after the terminal size settles (see its docs for why
/// that's the ceiling for terminals without Kitty's placement mechanism).
enum Watermark {
    Kitty(KittyMark),
    Compat(Box<Background>),
}

impl Watermark {
    fn load(picker: &mut Picker) -> anyhow::Result<Self> {
        if picker.protocol_type() == ProtocolType::Kitty {
            Ok(Self::Kitty(KittyMark::load(picker.font_size())?))
        } else {
            Ok(Self::Compat(Box::new(Background::load(picker)?)))
        }
    }

    /// No-op for `Kitty`: there's no background re-encode to poll for.
    fn poll_resize_result(&mut self) {
        if let Self::Compat(background) = self {
            background.poll_resize_result();
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect) {
        match self {
            Self::Kitty(mark) => mark.render(frame, area),
            Self::Compat(background) => background.render(frame, area),
        }
    }
}

fn main() -> anyhow::Result<()> {
    if relaunch_in_kitty_if_available() {
        return Ok(());
    }

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Must run after entering the alternate screen but before reading any
    // terminal events (it writes/reads stdio itself to detect the
    // available graphics protocol and font size).
    let mut picker = Picker::from_query_stdio()?;

    // iTerm2 answers Kitty's own capability query too, which steers
    // auto-detection onto `Kitty` even though iTerm2's actual rendering
    // support is for its own protocol, not Kitty's — that mismatch
    // silently renders nothing (encoding "succeeds" into a protocol
    // iTerm2 doesn't actually display). Prefer iTerm2's native protocol
    // whenever we know we're inside iTerm2.
    let is_iterm2 = std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "iTerm.app");
    if is_iterm2 && picker.protocol_type() == ProtocolType::Kitty {
        picker.set_protocol_type(ProtocolType::Iterm2);
    }

    // The query-based capability detection above can miss real Kitty
    // support too (falling back to the `Halfblocks` mosaic approximation
    // even inside actual Kitty), for the same class of reason the iTerm2
    // case above needs correcting by hand. `KITTY_WINDOW_ID` is set by
    // Kitty itself for every window it owns, which is a far more reliable
    // signal than a runtime query — and it's specifically what determines
    // whether `Watermark::load` picks the real-time `KittyMark` path below
    // at all, so getting this wrong silently falls all the way back to
    // `Background`'s ceiling instead of erroring loudly.
    let is_kitty = std::env::var("KITTY_WINDOW_ID").is_ok();
    if is_kitty && picker.protocol_type() != ProtocolType::Kitty {
        picker.set_protocol_type(ProtocolType::Kitty);
    }

    let result = Watermark::load(&mut picker).and_then(|watermark| run(&mut terminal, watermark));

    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;

    result
}

/// If Kitty is installed and we're not already running inside it, spawns
/// this same binary (with the same arguments) inside a fresh Kitty window
/// and returns `true` so `main` can exit immediately without touching the
/// current terminal at all. Once that new process starts, `KITTY_WINDOW_ID`
/// is set for it (Kitty sets this for whatever it runs), so it takes this
/// same path again, sees that, and just proceeds normally below — no risk
/// of relaunch looping.
///
/// Returns `false` (does nothing) if Kitty isn't installed, or if the
/// relaunch attempt itself fails for any reason — deliberately falls back
/// to running in the current terminal rather than installing Kitty or
/// erroring out. `Watermark::load` already degrades gracefully for
/// whatever terminal that turns out to be.
fn relaunch_in_kitty_if_available() -> bool {
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return false;
    }

    let Some(kitty) = find_kitty_binary() else {
        return false;
    };
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };

    Command::new(kitty)
        .arg(exe)
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Looks for a Kitty install: first on `PATH`, then at the standard macOS
/// `.app` bundle location — `brew install --cask kitty` puts the app there
/// without adding a `kitty` binary to `PATH`, so the `PATH` check alone
/// would miss the common case.
fn find_kitty_binary() -> Option<std::path::PathBuf> {
    if let Ok(output) = Command::new("which").arg("kitty").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }

    let app_bundle = std::path::Path::new("/Applications/kitty.app/Contents/MacOS/kitty");
    app_bundle.exists().then(|| app_bundle.to_path_buf())
}

/// Tracks the real (not simulated) `tart run` launch of the analysis VM,
/// polled once per tick until Tart reports either a VNC URL or the
/// process fails outright. Flat rather than an enum with `RunningVm`
/// embedded in variants — mutating a single field in place each tick is
/// simpler than moving `RunningVm` (which owns a `Child` and can't be
/// cloned) between enum variants.
struct VmLaunch {
    vm: Option<RunningVm>,
    vnc_url: Option<String>,
    error: Option<String>,
}

impl VmLaunch {
    fn launch(vm_name: &str, app_path: &std::path::Path, mode: ExplorationMode) -> Self {
        match RunningVm::launch(vm_name, app_path, mode) {
            Ok(vm) => Self {
                vm: Some(vm),
                vnc_url: None,
                error: None,
            },
            Err(e) => Self {
                vm: None,
                vnc_url: None,
                error: Some(e.to_string()),
            },
        }
    }

    /// Whether the launch has resolved one way or another (VNC ready, or
    /// failed) — while this is `false`, the notification sequence holds
    /// on the first message rather than advancing on a timer, since this
    /// step is now real instead of simulated.
    fn resolved(&self) -> bool {
        self.vnc_url.is_some() || self.error.is_some()
    }

    fn poll(&mut self) {
        if self.resolved() {
            return;
        }
        if let Some(vm) = &self.vm
            && let Some(url) = vm.poll_vnc_url()
        {
            self.vnc_url = Some(url);
        }
    }

    /// What the first notification line should actually say, reflecting
    /// real status instead of the static placeholder text the rest of the
    /// sequence still uses. The app is already staged and shared into the
    /// VM by the time `tart run` is spawned (see `vm::RunningVm::launch`),
    /// so there's no separate "transferring" step to report once this
    /// resolves — it's already true.
    fn status_line(&self) -> String {
        match (&self.vnc_url, &self.error) {
            (Some(url), _) => {
                format!("VM is up, the app is accessible via the shared directory — VNC: {url}")
            }
            (None, Some(err)) => format!("VM failed to start: {err}"),
            (None, None) => "VM is starting, transferring the app into it...".to_string(),
        }
    }
}

/// Where the session is in the fake end-to-end flow this prototype drives:
/// onboarding's 3 questions, then a small notification ticker standing in
/// for "VM opening / app transferring / exploring", then the placeholder
/// report streaming line-by-line into `command_line`'s real history, then
/// plain interactive use. Onboarding and Notifying both render into a
/// *fixed*-height area (see `onboarding::HEIGHT`/`notify::HEIGHT`) so the
/// watermark holds still through them; only Streaming and Interactive use
/// `command_line`'s own growing history, which is what actually shrinks the
/// watermark upward as report lines land — reusing that existing mechanism
/// rather than inventing a second one.
enum Stage {
    Onboarding(Onboarding),
    Notifying {
        messages: Vec<String>,
        shown: usize,
        last_tick: Instant,
        spinner_tick: usize,
    },
    /// `ExplorationMode::Unattended` only: no window is ever shown to
    /// the user (see `RunningVm::launch`'s `auto_open_vnc`) — instead this
    /// polls `events.jsonl` (via `AUTO_RUN_POLL`) for the target's own
    /// `exec`, watching for it to `exit` (success), never start within
    /// `AUTO_RUN_START_TIMEOUT`, or start but go quiet for longer than
    /// `AUTO_RUN_HANG_TIMEOUT` without exiting (stuck — an infinite loop
    /// or a wait on input that will never come). Whichever happens first
    /// triggers `finish_analysis` on its own, no Esc required — see
    /// `advance_stage`.
    AutoRunning {
        /// Learned from the first matching `exec` event once the target
        /// actually starts; `None` until then.
        target_pid: Option<i64>,
        started_at: Instant,
        /// Bumped forward every time a *new* sensor event for this
        /// target's process tree appears — the hang-detection clock runs
        /// off this, not off `started_at`, so a slow-but-genuinely-busy
        /// run isn't mistaken for a stall.
        last_activity_at: Instant,
        last_event_count: usize,
        spinner_tick: usize,
    },
    /// Between "the run is over" (Esc, or `AutoRunning` finishing on its
    /// own) and the real report existing — the Claude API call runs on a
    /// background thread (see `finish_analysis`) so the network round-trip
    /// never freezes the render loop, the same pattern `VmLaunch` already
    /// uses for the VNC-URL wait.
    GeneratingReport {
        rx: mpsc::Receiver<Vec<String>>,
        spinner_tick: usize,
    },
    Streaming {
        /// Lines not yet started.
        remaining_lines: VecDeque<String>,
        /// Characters left to type for the line currently in progress.
        current_chars: VecDeque<char>,
        /// Whether a line is mid-typewriter and still needs committing to
        /// history once `current_chars` drains (kept separate from
        /// `current_chars.is_empty()` so a genuinely blank report line
        /// still gets one commit rather than being skipped silently).
        current_line_active: bool,
        last_tick: Instant,
    },
    Interactive,
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut watermark: Watermark,
) -> anyhow::Result<()> {
    let mut command_line = CommandLine::default();
    let mut stage = Stage::Onboarding(Onboarding::default());
    // Lives for the rest of the process, not just `Stage::Notifying` — a
    // `Stage` transition used to drop this along with the old variant,
    // which (via `RunningVm`'s `Drop`) killed the VM the moment the
    // notification sequence finished, seconds after it opened. Scoping it
    // here instead means it only tears down when `run` itself returns.
    let mut vm: Option<VmLaunch> = None;
    // Set alongside `vm` when the VM launches, kept around through every
    // later stage (unlike `Answers`, which `Stage::Streaming`/`Interactive`
    // don't carry) so `finish_analysis` still knows what to resolve
    // `vm::expected_guest_exec_path` against once the user is done.
    let mut target_app_path: Option<PathBuf> = None;
    // Set alongside `target_app_path` — `advance_stage` needs it to know
    // whether a finished `Notifying` ticker should sit idle (Manual, wait
    // for the user's Esc) or auto-transition into `Stage::AutoRunning`
    // (Unattended, no human ever touches this VM).
    let mut exploration_mode: Option<ExplorationMode> = None;

    loop {
        watermark.poll_resize_result();
        advance_stage(
            &mut stage,
            &mut vm,
            &mut command_line,
            target_app_path.as_deref(),
            exploration_mode,
        );

        terminal.draw(|frame| {
            let area = frame.area();
            match &stage {
                Stage::Onboarding(onboarding) => {
                    let [watermark_area, panel_area] = Layout::vertical([
                        Constraint::Min(0),
                        Constraint::Length(insula::cli::onboarding::HEIGHT),
                    ])
                    .areas(area);
                    watermark.render(frame, watermark_area);
                    onboarding.render(frame, panel_area);
                }
                Stage::Notifying {
                    messages,
                    shown,
                    spinner_tick,
                    ..
                } => {
                    let Some(vm) = &vm else { return };
                    let [watermark_area, panel_area] = Layout::vertical([
                        Constraint::Min(0),
                        Constraint::Length(insula::cli::notify::HEIGHT),
                    ])
                    .areas(area);
                    watermark.render(frame, watermark_area);

                    // Index 0 is the real VM-launch status line
                    // (`vm.status_line()`), not a canned string like the
                    // rest of `messages` — see `VmLaunch`.
                    let total = messages.len() + 1;
                    let shown = (*shown).min(total);
                    let mut past: Vec<String> = Vec::new();
                    if shown >= 1 {
                        past.push(vm.status_line());
                    }
                    if shown >= 2 {
                        past.extend(messages[..shown - 1].iter().cloned());
                    }
                    let current = if shown == 0 {
                        Some(vm.status_line())
                    } else {
                        messages.get(shown - 1).cloned()
                    };

                    Notifications {
                        past: &past,
                        current: current.as_deref(),
                        spinner_tick: *spinner_tick,
                    }
                    .render(frame, panel_area);
                }
                Stage::AutoRunning {
                    target_pid,
                    spinner_tick,
                    ..
                } => {
                    let [watermark_area, panel_area] = Layout::vertical([
                        Constraint::Min(0),
                        Constraint::Length(insula::cli::notify::HEIGHT),
                    ])
                    .areas(area);
                    watermark.render(frame, watermark_area);
                    let current = match target_pid {
                        Some(pid) => format!("App is running in the background (pid {pid}), observing..."),
                        None => "Starting the app in the background...".to_string(),
                    };
                    Notifications {
                        past: &[],
                        current: Some(&current),
                        spinner_tick: *spinner_tick,
                    }
                    .render(frame, panel_area);
                }
                Stage::GeneratingReport { spinner_tick, .. } => {
                    let [watermark_area, panel_area] = Layout::vertical([
                        Constraint::Min(0),
                        Constraint::Length(insula::cli::notify::HEIGHT),
                    ])
                    .areas(area);
                    watermark.render(frame, watermark_area);
                    Notifications {
                        past: &[],
                        current: Some("Claude is interpreting the observations..."),
                        spinner_tick: *spinner_tick,
                    }
                    .render(frame, panel_area);
                }
                Stage::Streaming { .. } | Stage::Interactive => {
                    // Reserve the bottom region for the command line
                    // *before* the watermark ever sees the frame area, so
                    // its placement is confined to the area above and never
                    // shares a cell with the command line — see
                    // `command_line`'s module docs for why drawing the two
                    // on top of each other isn't safe for a real
                    // graphics-protocol mark. Sized to what's actually
                    // accumulated (capped at 70% of the frame so the mark
                    // always keeps some room), not a fixed height — see
                    // `CommandLine::needed_height`.
                    let max_flow_height = (area.height * 7) / 10;
                    let flow_height = command_line.needed_height(max_flow_height);
                    let [watermark_area, command_line_area] =
                        Layout::vertical([Constraint::Min(0), Constraint::Length(flow_height)])
                            .areas(area);

                    watermark.render(frame, watermark_area);
                    command_line.render(frame, command_line_area);
                }
            }
        })?;

        let poll_timeout = match stage {
            Stage::Notifying { .. } | Stage::GeneratingReport { .. } | Stage::Streaming { .. } => {
                ANIMATION_POLL
            }
            Stage::AutoRunning { .. } => AUTO_RUN_POLL,
            Stage::Onboarding(_) | Stage::Interactive => IDLE_POLL,
        };
        if event::poll(poll_timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Esc {
                        // First Esc while a VM is still up tears it down and
                        // shows what was actually observed instead of
                        // quitting outright — see `finish_analysis`. Once
                        // there's nothing left to finish (already torn down,
                        // or the launch never produced a VM at all), Esc
                        // goes back to just quitting.
                        if finish_analysis(&mut vm, target_app_path.as_deref(), None, &mut stage) {
                            continue;
                        }
                        return Ok(());
                    }
                    match &mut stage {
                        Stage::Onboarding(onboarding) => {
                            if let Some(answers) = onboarding.handle_key(key) {
                                let messages = notification_messages(&answers);
                                target_app_path = Some(PathBuf::from(&answers.path));
                                exploration_mode = Some(answers.mode);
                                vm = Some(VmLaunch::launch(
                                    GOLDEN_VM_NAME,
                                    Path::new(&answers.path),
                                    answers.mode,
                                ));
                                stage = Stage::Notifying {
                                    messages,
                                    shown: 0,
                                    last_tick: Instant::now(),
                                    spinner_tick: 0,
                                };
                            }
                        }
                        Stage::Notifying { .. }
                        | Stage::AutoRunning { .. }
                        | Stage::GeneratingReport { .. }
                        | Stage::Streaming { .. } => {}
                        Stage::Interactive => {
                            command_line.handle_key(key);
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    if let Stage::Interactive = stage {
                        command_line.handle_mouse(mouse);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Drives the timer-based transitions between stages: advancing the
/// notification ticker, polling a headless `Unattended` run for
/// completion/hang/crash, streaming report lines into `command_line`'s
/// real history, and moving to the next stage once each phase runs out of
/// content. Split from rendering so the borrow of whichever `Stage` variant
/// is active never overlaps with reassigning `*stage` itself.
fn advance_stage(
    stage: &mut Stage,
    vm: &mut Option<VmLaunch>,
    command_line: &mut CommandLine,
    target_app_path: Option<&Path>,
    exploration_mode: Option<ExplorationMode>,
) {
    // Populated inside the match below (which borrows `stage`) and acted
    // on after it ends, mirroring the notifying_done/streaming_done
    // transition checks further down — `*stage` can't be reassigned while
    // still borrowed by the arm that's deciding to reassign it.
    let mut generated_report: Option<Vec<String>> = None;
    // Set when `AutoRunning`'s polling decides the run is over (target
    // exited, never started, or went quiet too long) — holds the Turkish
    // finding text `finish_analysis` folds into the report.
    let mut generated_finish: Option<String> = None;

    match stage {
        Stage::GeneratingReport { rx, spinner_tick, .. } => {
            *spinner_tick = spinner_tick.wrapping_add(1);
            if let Ok(lines) = rx.try_recv() {
                generated_report = Some(lines);
            }
        }
        Stage::AutoRunning {
            target_pid,
            started_at,
            last_activity_at,
            last_event_count,
            spinner_tick,
        } => {
            *spinner_tick = spinner_tick.wrapping_add(1);
            if let Some(events) = vm.as_ref().and_then(|v| {
                let running = v.vm.as_ref()?;
                let target = target_app_path?;
                let exec_path = insula::vm::expected_guest_exec_path(target)?;
                let raw = std::fs::read_to_string(running.logs_dir().join("events.jsonl"))
                    .unwrap_or_default();
                Some(insula::event_filter::filter_for_target(
                    &raw,
                    &exec_path.to_string_lossy(),
                ))
            }) {
                if events.len() != *last_event_count {
                    *last_event_count = events.len();
                    *last_activity_at = Instant::now();
                }
                if target_pid.is_none()
                    && let Some(pid) = events.first().and_then(|e| e["pid"].as_i64())
                {
                    *target_pid = Some(pid);
                }

                let exit_status = target_pid.and_then(|pid| {
                    events
                        .iter()
                        .find(|e| e["type"] == "exit" && e["pid"].as_i64() == Some(pid))
                        .and_then(|e| e["status"].as_i64())
                });

                generated_finish = if let Some(status) = exit_status {
                    Some(format!(
                        "The target app exited on its own (pid {}, exit status: {status}).",
                        target_pid.unwrap_or(-1)
                    ))
                } else if target_pid.is_none() && started_at.elapsed() > AUTO_RUN_START_TIMEOUT {
                    Some(format!(
                        "The target app never started within {} seconds — the auto-run daemon \
                         may have failed, or the app isn't running from the expected path.",
                        AUTO_RUN_START_TIMEOUT.as_secs()
                    ))
                } else if target_pid.is_some() && last_activity_at.elapsed() > AUTO_RUN_HANG_TIMEOUT
                {
                    Some(format!(
                        "The target app (pid {}) started but hasn't produced any new events for \
                         {} seconds and hasn't exited — it's likely stuck in an infinite loop or \
                         waiting on input that will never arrive.",
                        target_pid.unwrap_or(-1),
                        AUTO_RUN_HANG_TIMEOUT.as_secs()
                    ))
                } else {
                    None
                };
            }
        }
        Stage::Notifying {
            messages,
            shown,
            last_tick,
            spinner_tick,
            ..
        } => {
            *spinner_tick = spinner_tick.wrapping_add(1);
            let Some(vm) = vm else { return };
            vm.poll();
            // Index 0 (the real VM-launch line) only advances once the
            // launch has actually resolved — everything after it is still
            // timer-based placeholder text.
            let can_advance = *shown > 0 || vm.resolved();
            let total = messages.len() + 1;
            if can_advance && *shown < total && last_tick.elapsed() >= NOTIFY_INTERVAL {
                *shown += 1;
                *last_tick = Instant::now();
            }
        }
        Stage::Streaming {
            remaining_lines,
            current_chars,
            current_line_active,
            last_tick,
        } => {
            if last_tick.elapsed() >= CHAR_INTERVAL {
                *last_tick = Instant::now();
                if let Some(c) = current_chars.pop_front() {
                    command_line.push_streaming_char(c);
                } else if *current_line_active {
                    command_line.commit_streaming_line();
                    *current_line_active = false;
                } else if let Some(next) = remaining_lines.pop_front() {
                    if next.is_empty() {
                        // Nothing to type-write for a blank spacer line —
                        // commit it immediately rather than burning a tick
                        // waiting on zero characters.
                        command_line.commit_streaming_line();
                    } else {
                        *current_chars = next.chars().collect();
                        *current_line_active = true;
                    }
                }
            }
        }
        Stage::Onboarding(_) | Stage::Interactive => {}
    }

    if let Some(lines) = generated_report {
        *stage = Stage::Streaming {
            remaining_lines: lines.into(),
            current_chars: VecDeque::new(),
            current_line_active: false,
            last_tick: Instant::now(),
        };
    }

    if let Some(finding) = generated_finish {
        finish_analysis(vm, target_app_path, Some(finding), stage);
    }

    // `Notifying`'s ticker plays through its canned messages once, then:
    // `Manual` just holds on the last one — the VM stays up and the user
    // is free to explore it for as long as they want, `Stage::Streaming`
    // only ever entered via `finish_analysis` (Esc) from here on.
    // `Unattended` instead auto-transitions into `Stage::AutoRunning` —
    // nobody is watching this VM, so there's nothing to hold for.
    let notifying_ticker_done = matches!(
        stage,
        Stage::Notifying { messages, shown, .. } if *shown > messages.len()
    );
    if notifying_ticker_done && exploration_mode == Some(ExplorationMode::Unattended) {
        *stage = Stage::AutoRunning {
            target_pid: None,
            started_at: Instant::now(),
            last_activity_at: Instant::now(),
            last_event_count: 0,
            spinner_tick: 0,
        };
    }

    let streaming_done = matches!(
        stage,
        Stage::Streaming { remaining_lines, current_chars, current_line_active, .. }
            if remaining_lines.is_empty() && current_chars.is_empty() && !*current_line_active
    );
    if streaming_done {
        *stage = Stage::Interactive;
    }
}

/// The status line shown after the real VM-launch line (`VmLaunch::status_line`,
/// which already covers the app transfer — see its docs) while
/// `Stage::Notifying` is active. The second line is an instruction, not a
/// claim that a report is already being produced — nothing happens
/// automatically anymore, see `finish_analysis`.
fn notification_messages(answers: &Answers) -> Vec<String> {
    vec![
        match answers.mode {
            ExplorationMode::Manual => {
                "VM is ready — you can explore the app yourself inside it.".to_string()
            }
            ExplorationMode::Unattended => {
                "Running the app unattended in the background and observing it...".to_string()
            }
        },
        "When you're done exploring, come back here and press Esc — the observation report will be prepared.".to_string(),
    ]
}

/// Tears the VM down (if one is still up) and switches `stage` into
/// `Stage::GeneratingReport`, which builds the real report — including a
/// Claude API call — on a background thread rather than blocking the
/// render loop. Returns `false` (does nothing) if there's no active VM to
/// finish — either the launch never produced one, or a previous call to
/// this function already did the teardown — so the caller knows to let
/// Esc actually quit instead.
fn finish_analysis(
    vm: &mut Option<VmLaunch>,
    target_app_path: Option<&Path>,
    auto_finding: Option<String>,
    stage: &mut Stage,
) -> bool {
    let Some(running_vm) = vm.as_mut().and_then(|v| v.vm.take()) else {
        return false;
    };
    // Read the logs path out before dropping — `Drop` tears the VM down
    // (and its staging dir) but deliberately leaves `logs_dir` itself on
    // disk, see `vm.rs`'s module doc comment for why.
    let logs_dir = running_vm.logs_dir().to_path_buf();
    drop(running_vm);

    let target_app_path = target_app_path.map(Path::to_path_buf);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let lines =
            build_real_report_lines(&logs_dir, target_app_path.as_deref(), auto_finding.as_deref());
        let _ = tx.send(lines);
    });

    *stage = Stage::GeneratingReport { rx, spinner_tick: 0 };
    true
}

/// Builds the report from `insula_sensor`'s real, captured `events.jsonl`,
/// filtered down to just the submitted app's own process tree via
/// `event_filter::filter_for_target` — see that module's docs for why
/// filtering happens here (downstream, after the fact) rather than live in
/// the sensor. `auto_finding` carries `Stage::AutoRunning`'s own
/// completion/timeout/hang verdict (`None` for a manual, Esc-ended run) —
/// a hard fact from sensor data, not something that needs an LLM to
/// determine, so it's surfaced directly rather than left for Claude to
/// re-guess from the raw events alone.
fn build_real_report_lines(
    logs_dir: &Path,
    target_app_path: Option<&Path>,
    auto_finding: Option<&str>,
) -> Vec<String> {
    let mut lines = vec![String::new()];

    let Some(target_app_path) = target_app_path else {
        lines.push("Couldn't generate the observation report: the target app path is missing.".to_string());
        return lines;
    };
    lines.push(format!(
        "=== Insula Observation Report — {} ===",
        target_app_path.display()
    ));
    lines.push(String::new());

    if let Some(finding) = auto_finding {
        lines.push("-- Auto-Run Finding --".to_string());
        lines.push(finding.to_string());
        lines.push(String::new());
    }

    let Some(target_exec_path) = insula::vm::expected_guest_exec_path(target_app_path) else {
        lines.push(
            "This path doesn't look like a runnable app, events couldn't be filtered."
                .to_string(),
        );
        return lines;
    };
    let target_exec_path = target_exec_path.to_string_lossy().into_owned();

    let raw = std::fs::read_to_string(logs_dir.join("events.jsonl")).unwrap_or_default();
    let events = insula::event_filter::filter_for_target(&raw, &target_exec_path);

    lines.push(format!("Target exec path (inside the VM): {target_exec_path}"));
    lines.push(format!("Total observed events: {}", events.len()));
    lines.push(String::new());

    if events.is_empty() {
        lines.push(
            "No record was found of the target app ever running — this is normal if you"
                .to_string(),
        );
        lines.push(
            "never started the app inside the VM, or the VM was closed before the sensor"
                .to_string(),
        );
        lines.push("was ready.".to_string());
        return lines;
    }

    match insula::claude_report::narrate(&events, target_app_path, auto_finding.map(str::to_string)) {
        Ok(narrative) => {
            lines.push("-- Claude Analysis --".to_string());
            lines.extend(narrative);
        }
        Err(reason) => {
            lines.push(format!(
                "-- Note: couldn't get a Claude analysis ({reason}) — showing the raw timeline instead --"
            ));
            lines.push(String::new());
            lines.push("-- Timeline --".to_string());
            let base_time = events[0]["time_unix_secs"].as_i64().unwrap_or(0);
            for event in &events {
                let offset = event["time_unix_secs"].as_i64().unwrap_or(base_time) - base_time;
                lines.push(format!("+{offset:>3}s  {}", describe_event(event)));
            }
        }
    }

    lines.push(String::new());
    lines.push("-- Note --".to_string());
    lines.push(
        "This report is based solely on real events belonging to the target app's own"
            .to_string(),
    );
    lines.push("process tree, filtered (via event_filter) from the raw sensor log.".to_string());

    lines
}

/// Plain-language description of one filtered event, matched by `"type"`
/// — mirrors the event shapes `insula_sensor.rs`'s `describe_event`
/// actually produces.
fn describe_event(event: &serde_json::Value) -> String {
    let pid = event["pid"].as_i64().unwrap_or(-1);
    match event["type"].as_str().unwrap_or("") {
        "exec" => format!(
            "Process started (pid {pid}): {}",
            event["path"].as_str().unwrap_or("?")
        ),
        "fork" => format!(
            "New process spawned (pid {pid} → child {})",
            event["child_pid"].as_i64().unwrap_or(-1)
        ),
        "exit" => format!(
            "Process exited (pid {pid}), exit status: {}",
            event["status"].as_i64().unwrap_or(-1)
        ),
        "create" => format!(
            "File created (pid {pid}): {}",
            event["path"].as_str().unwrap_or("?")
        ),
        "rename" => format!(
            "File renamed (pid {pid}): {} → {}",
            event["source_path"].as_str().unwrap_or("?"),
            event["destination_path"].as_str().unwrap_or("?"),
        ),
        "unlink" => format!(
            "File deleted (pid {pid}): {}",
            event["path"].as_str().unwrap_or("?")
        ),
        other => format!("Unknown event type ({other}), pid {pid}"),
    }
}
