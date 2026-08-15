# Insula — Status (read this first in a new session)

This file exists so a new Claude session can pick up exactly where the
last one left off, without re-deriving context. Read this, then the doc(s)
it points to for whatever you're about to work on — don't re-read
everything from scratch every time.

## What Insula is, in one paragraph

A consent-gated, VM-isolated dynamic analysis tool: the user hands Insula
any app they want to understand, Insula talks to them about what they
actually want to know, runs the app in complete isolation (explored either
manually by the user or agentically by Claude via VM input injection),
observes everything it does, and delivers a report the user can interrogate
conversationally — plus, optionally, an honestly-labeled, confidence-tiered
*inferred structure sketch* of how the app appears to work (never a claim
of extracted or decompiled source code). Full vision: `PROJECT.md`. Full
technical pipeline (Stage 0-10): `ARCHITECTURE.md`. v1/v2 staging
decisions: `ROADMAP.md`. Unresolved design questions: `OPEN_QUESTIONS.md`.

**Settled, don't re-litigate:** Insula is a reverse-engineering / dynamic
analysis tool, not a security-verdict product. There's no risky-download
interception, no OS-level "run a security check?" notification, no
release/block decision anywhere in the design. If anything you find
references a "quarantine," a "trigger" tied to a flagged download, or a
malicious/clean verdict, it's stale — `PROJECT.md`/`ARCHITECTURE.md` are
the current source of truth.

## Where things actually stand

**Design docs** (`PROJECT.md`, `ARCHITECTURE.md`, `ROADMAP.md`,
`OPEN_QUESTIONS.md`) — internally consistent as of this session, cover the
whole pipeline end to end: app submission → conversational intake → VM →
exploration (manual or Claude-driven) → observation → evidence extraction
→ notability scoring → AI-assisted synthesis → report → teardown. Read
`PROJECT.md`'s "Core concept" before assuming anything needs re-deciding.

**Milestone 1 (Static Profiling Engine) — built, tested, working.** This is
the only milestone with both a task breakdown
(`tasks/milestone-1-detection-engine/` — folder name predates the current
milestone name) and actual code (`src/`, Rust). Status in detail:
`tasks/README.md`. Its extracted signals (entropy, strings, signature
checks, file-type classification, source reputation) feed
`ARCHITECTURE.md` Stage 6's static structural profile directly.

- `cargo build` — clean, zero warnings. `cargo test` — 39 passing tests.
  `cargo clippy` — clean.
- Live-tested against real simulated inputs (benign file, disguised
  Mach-O, RTL-override filename, double extension, pipe-to-shell script,
  zip/tar with nested high-notability entries, high-entropy content,
  ad-hoc-signed binary, a symlink disguised as a normal file, duplicate
  filenames on separate occasions) — all produced the expected result.
- Two real bugs were found live and fixed with regression tests: a
  correctness bug in the file-locking mechanism (symlinks could have
  caused Insula to modify permissions on an arbitrary file outside its
  remit — fixed) and a functional one (a second file reusing an earlier
  filename was silently ignored forever — fixed). Both documented in
  `tasks/README.md`.
- **Hard invariant, honored throughout:** nothing in this codebase ever
  executes, opens, or invokes a submitted file on the host. Every check is
  static — bytes, metadata, structure only. (Actually running the app is
  the VM's job, starting Milestone 3 — this invariant is about the host.)

**Milestone 2 (Consent & Launch) — CLI shell substantially built this
session, still no formal task breakdown.** No `milestone-2-consent-launch/`
task files exist yet — worth writing before going further. What exists,
all built this session:

- `src/bin/insula_cli.rs` — CLI entry point. **Self-relaunches inside
  Kitty** on startup if Kitty is installed but isn't already the current
  terminal (checks `PATH` and the standard `/Applications/kitty.app`
  bundle location), so the same real-time experience is available
  regardless of which terminal was open — falls back gracefully (no
  install, no error) if Kitty isn't present.
- `src/cli/kitty_mark.rs` — **hand-rolled Kitty graphics-protocol
  implementation** for the background watermark. Transmits the mark image
  once, then every subsequent resize only sends a cheap placement-size
  update (`a=p` referencing the same image/placement id) — true real-time
  resize tracking, confirmed live. This had to be hand-rolled because
  `ratatui_image`'s own `StatefulKitty` re-transmits the *entire* image on
  every resize (its own source comment says so) and can't do this; that
  gap is exactly what this module's doc comment explains.
- `src/cli/background.rs` — fallback watermark renderer for non-Kitty
  terminals (iTerm2, Sixel, Terminal.app's half-block approximation, still
  the real ceiling there, not a bug): settle-then-redraw after ~150ms of
  size stability, not real-time, since those protocols have no equivalent
  of Kitty's cheap placement-resize mechanism — confirmed via
  `ratatui-image`'s actual iTerm2 encoder source that a full PNG
  re-encode+retransmit is unavoidable there. Also fixes two real bugs found
  this session: a visible seam from unfilled resize padding (fixed via
  `Picker::set_background_color`), and the mark visibly flashing/vanishing
  mid-resize (fixed by freezing the last-rendered frame in place — not
  recentering it — while a resize is in flight, since recentering a
  Kitty/iTerm2 image cell forces the terminal to retransmit its entire
  payload).
- `src/cli/command_line.rs` — a classic, borderless, terminal-flow command
  input reserved below the watermark (never drawn *over* it — see the
  module's doc comment for why that's unsafe for a real graphics-protocol
  cell). Dynamically sized (starts at one line, grows with typed history,
  capped at 70% of the frame), `Up`/`Down`/`PageUp`/`PageDown` and
  mouse-wheel scroll through history, snaps back to the live bottom on new
  input or submit.
- `Cargo.toml` also gained a `[profile.dev.package]` override
  (`image`/`png`/compression crates at `opt-level = 3`) — debug builds of
  the resize/encode path were ~20-30x slower than release without it
  (measured: ~6.4s vs ~170ms at large sizes), which is what earlier
  "resize feels laggy" reports in this session actually traced back to.

**Still missing:** Stage 1 (app submission — mechanics still open,
`OPEN_QUESTIONS.md` §8) and Stage 2 (the conversational intake — what do
you want to know, manual or Claude-driven exploration) don't exist yet.
`cargo test` is still just Milestone 1's 39 — none of this session's CLI
work has tests. `Background`/`KittyMark` depend on live terminal protocol
detection and real terminal-graphics rendering, which isn't meaningfully
testable headless the way Milestone 1's tasks were; this is a real,
acknowledged gap versus Milestone 1's rigor, not a deliberate call.

The CLI work so far is the visual/interactive shell Stages 1/2 will be
built on top of — none of the actual new pipeline stages exist yet.

**Milestones 3-6** (Isolated Delivery, Observation Mode, Live CLI
Streaming, Final Report) — no task breakdown exists yet, no code exists
yet. Milestone 4 covers the manual/Claude-driven exploration modes;
Milestone 6 covers the full evidence report + conversational Q&A layer +
confidence-tiered inferred structure sketch.

## Known loose ends — true as of this session, check before trusting

- `INSULA_SAFE_BROWSING_API_KEY` is not set anywhere. Without it, task 1.2
  always reports `Unknown` (degrades gracefully). Left unresolved
  deliberately — don't "fix" this unprompted. If picking this up: the code
  already reads the env var, no code change needed, just `export
  INSULA_SAFE_BROWSING_API_KEY=...` before running.
- The binary is currently started manually, not installed as a
  `LaunchAgent`. Whether that's still needed at all is now a real question
  — a persistent background service made sense for a download-interception
  flow that no longer exists; whether Insula needs to run as a background
  service for the current submission-driven design hasn't been revisited.
- `OPEN_QUESTIONS.md` has eight open items (decoy VM generation,
  delayed-trigger/clock-acceleration mitigation, VM tooling, guest OS,
  implementation language, VM-exploration input-injection mechanics §6,
  inferred-structure-sketch confidence validation §7, app-submission CLI
  mechanics §8) — none block current work, none have been revisited since
  being written.
- **Nothing has been committed to git yet.** `cargo init` created the repo
  but there's no commit history — everything in the working tree is
  uncommitted. Worth doing before this grows further, but wasn't asked for
  during this session so it wasn't done unprompted.

## If you're picking this up cold

Read `PROJECT.md`'s "Core concept" section first if you haven't already —
it's the whole shape of what Insula is and why.

Milestone 2's CLI is mid-flight and now visually solid (Kitty-native
real-time watermark + scrollable terminal-flow command line) but still
purely a shell — none of it is wired to any real backend, and neither
Stage 1 (app submission) nor Stage 2 (conversational intake) exists yet.
The natural next step is either: (a) writing a real task breakdown for
Milestone 2 (app submission mechanics per `OPEN_QUESTIONS.md` §8, and the
Stage 2 intake conversation), following the design-first process
Milestone 1 used, or (b) resolving one of the other open questions
(§6/§7) in more depth before committing to an implementation. Don't assume
the CLI's current watermark-only behavior is an oversight — it's a
deliberately incremental visual foundation, built and confirmed piece by
piece this session.
