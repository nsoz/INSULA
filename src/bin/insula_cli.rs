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
use std::process::{Command, Stdio};
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

/// Name of the Tart VM clone Insula boots for analysis — currently a
/// fixed, pre-cloned instance (`tart clone
/// ghcr.io/cirruslabs/macos-tahoe-base:latest insula-macos`); a real
/// clone-per-run lifecycle is later Milestone-3 work, see
/// `project-insula-vm-tooling` memory.
const VM_NAME: &str = "insula-macos";

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
        let auto_open_vnc = matches!(mode, ExplorationMode::Manual);
        match RunningVm::launch(vm_name, app_path, auto_open_vnc) {
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
                format!("VM açıldı, uygulama paylaşımlı dizin üzerinden erişilebilir — VNC: {url}")
            }
            (None, Some(err)) => format!("VM açılamadı: {err}"),
            (None, None) => "VM açılıyor, uygulama VM'e aktarılıyor...".to_string(),
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
        answers: Answers,
        messages: Vec<String>,
        shown: usize,
        last_tick: Instant,
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

    loop {
        watermark.poll_resize_result();
        advance_stage(&mut stage, &mut vm, &mut command_line);

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
            Stage::Notifying { .. } | Stage::Streaming { .. } => ANIMATION_POLL,
            Stage::Onboarding(_) | Stage::Interactive => IDLE_POLL,
        };
        if event::poll(poll_timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Esc {
                        return Ok(());
                    }
                    match &mut stage {
                        Stage::Onboarding(onboarding) => {
                            if let Some(answers) = onboarding.handle_key(key) {
                                let messages = notification_messages(&answers);
                                vm = Some(VmLaunch::launch(
                                    VM_NAME,
                                    std::path::Path::new(&answers.path),
                                    answers.mode,
                                ));
                                stage = Stage::Notifying {
                                    answers,
                                    messages,
                                    shown: 0,
                                    last_tick: Instant::now(),
                                    spinner_tick: 0,
                                };
                            }
                        }
                        Stage::Notifying { .. } | Stage::Streaming { .. } => {}
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
/// notification ticker, streaming report lines into `command_line`'s real
/// history, and moving to the next stage once each phase runs out of
/// content. Split from rendering so the borrow of whichever `Stage` variant
/// is active never overlaps with reassigning `*stage` itself.
fn advance_stage(stage: &mut Stage, vm: &mut Option<VmLaunch>, command_line: &mut CommandLine) {
    match stage {
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

    let notifying_done = matches!(
        stage,
        Stage::Notifying { messages, shown, .. } if *shown > messages.len()
    );
    if notifying_done && let Stage::Notifying { answers, .. } = stage {
        let remaining_lines: VecDeque<String> = build_report_lines(answers).into();
        *stage = Stage::Streaming {
            remaining_lines,
            current_chars: VecDeque::new(),
            current_line_active: false,
            last_tick: Instant::now(),
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

/// The fake "exploring" status line shown after the real VM-launch line
/// (`VmLaunch::status_line`, which already covers the app transfer — see
/// its docs) while `Stage::Notifying` is active — standing in for the
/// real Milestone 4 exploration pipeline, which doesn't exist yet.
fn notification_messages(answers: &Answers) -> Vec<String> {
    vec![
        match answers.mode {
            ExplorationMode::Manual => {
                "VM hazır — uygulamayı VM içinde kendiniz keşfedebilirsiniz.".to_string()
            }
            ExplorationMode::ClaudeAgentic => {
                "Claude, erişilebilirlik ağacı üzerinden uygulamayı VM içinde inceliyor..."
                    .to_string()
            }
        },
        "Gözlem tamamlandı, rapor hazırlanıyor...".to_string(),
    ]
}

/// A placeholder report, clearly labeled as such — Milestones 3-6 (the real
/// VM/observation/report pipeline) don't exist yet. Only here to exercise
/// the line-by-line streaming into `command_line`'s history, which is what
/// actually drives the watermark's upward shrink.
fn build_report_lines(answers: &Answers) -> Vec<String> {
    let mode_label = match answers.mode {
        ExplorationMode::Manual => "Manuel",
        ExplorationMode::ClaudeAgentic => "Claude",
    };
    vec![
        String::new(),
        format!("=== Insula Gözlem Raporu — {} ===", answers.path),
        String::new(),
        "Gözlem penceresi: VM boot'undan itibaren kesintisiz, transfer ve keşif dahil tek parça."
            .to_string(),
        format!("Keşif modu: {mode_label}"),
        "Toplam gözlem süresi: 00:04:37".to_string(),
        String::new(),
        "-- Özet --".to_string(),
        "Uygulama başlangıçta beklenen dosya/dizin erişimlerini yaptı; ardından amacıyla"
            .to_string(),
        "doğrudan ilişkisi görünmeyen birkaç sistem sorgusu ve bir ağ bağlantı denemesi"
            .to_string(),
        "gözlemlendi. Aşağıdaki zaman çizelgesi ve anomali bölümü bunları ayrı ayrı listeliyor."
            .to_string(),
        String::new(),
        "-- Statik Yapı Profili --".to_string(),
        "Dosya türü: Mach-O 64-bit executable (arm64)".to_string(),
        "İmzalama: ad-hoc, tanınan bir yayıncı sertifikası yok.".to_string(),
        "Entitlement'lar: com.apple.security.network.client, sandbox devre dışı.".to_string(),
        "Bölüm entropisi: __TEXT 5.9, __DATA 4.1, __LINKEDIT 6.2 — paketleme belirtisi yok."
            .to_string(),
        "Bağlı kütüphaneler: libSystem, CoreFoundation, Security, libcurl.".to_string(),
        "Gömülü stringler arasında 2 URL, 1 API anahtarı benzeri yüksek entropili dizgi var."
            .to_string(),
        String::new(),
        "-- Davranış Zaman Çizelgesi --".to_string(),
        "00:00:02  Süreç başlatıldı, ana thread çalışmaya başladı.".to_string(),
        "00:00:03  Dosya: kendi bulunduğu dizini okudu (kod imzası doğrulama olabilir)."
            .to_string(),
        "00:00:05  Sistem: toplam çalışma süresi (uptime) sorgulandı.".to_string(),
        "00:00:07  Dosya: kullanıcının Belgeler klasörü listelendi.".to_string(),
        "00:00:09  Dosya: Masaüstü klasörü listelendi (uygulamanın amacıyla ilişkisi belirsiz)."
            .to_string(),
        "00:00:12  Süreç: adı bilinmeyen bir yardımcı süreç başlatıldı, kısa sürede sonlandı."
            .to_string(),
        "00:00:18  Ağ: dış bir sunucuya bağlantı denemesi (yanıt yok, VM izole).".to_string(),
        "00:00:26  Sistem: yüklü uygulamalar listesi sorgulandı (VM/analiz araçları dahil)."
            .to_string(),
        "00:00:41  Dosya: geçici dizine küçük bir dosya yazıldı, ardından silindi.".to_string(),
        "00:01:03  Süreç kendi çalışma sayacını (run-count) okudu, artırdı, kapattı.".to_string(),
        String::new(),
        "-- Anomali / Kaçınma Sinyalleri --".to_string(),
        "[dikkat] Yüklü uygulama taraması, bilinen sanallaştırma/analiz araçlarının adlarını"
            .to_string(),
        "         içeriyordu — VM tespiti amaçlı olabilir.".to_string(),
        "[dikkat] Çalışma sayacı okuma+artırma döngüsü, gecikmeli tetikleyici (delayed-trigger)"
            .to_string(),
        "         davranışıyla tutarlı; bu oturumda payload tetiklenmedi.".to_string(),
        "[bilgi]  Masaüstü klasörü taraması, uygulamanın belirttiği amaçla doğrudan ilişkili"
            .to_string(),
        "         görünmüyor — amaç dışı numaralandırma (irrelevant enumeration).".to_string(),
        String::new(),
        "-- Bilinen Desen Eşleşmeleri --".to_string(),
        "Yüksek entropili gömülü dizgi, bilinen bir ticari çökme-raporlama SDK'sının anahtar"
            .to_string(),
        "biçimiyle kısmen örtüşüyor (orta güven).".to_string(),
        String::new(),
        "-- Çıkarılan Yapı Taslağı (deneysel) --".to_string(),
        "[doğrulanmış]  Ağ bağlantı denemesi sabit bir host:port çiftine yapılıyor — 3 farklı"
            .to_string(),
        "               girdiyle tekrarlanan testte davranış değişmedi.".to_string(),
        "[çıkarım]      Çalışma sayacı belirli bir eşiği geçtiğinde ek bir davranışın".to_string(),
        "               tetiklenebileceği düşünülüyor; bu oturumda doğrulanamadı.".to_string(),
        String::new(),
        "-- Not --".to_string(),
        "Bu gerçek bir VM oturumu değil; CLI akışını göstermek için yer tutucu bir örnek"
            .to_string(),
        "rapordur. Gerçek gözlem/rapor motoru Milestone 3-6 kapsamında henüz yazılmadı."
            .to_string(),
        String::new(),
        "Bu kanıtlarla ilgili sorularınızı aşağıya yazabilirsiniz.".to_string(),
    ]
}
