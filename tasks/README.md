# Insula — Tasks

Superseded its original 00-13 flat numbering — the project is now broken
into **milestones** (see the six defined during design discussion: Static
Profiling Engine, Consent & Launch, Isolated Delivery, Observation Mode,
Live CLI Streaming, Final Report), each with its own numbered task files
in a `milestone-N-*/` subfolder. Only Milestone 1 has been designed and
built so far.

## Milestone 1 — Static Profiling Engine

`milestone-1-detection-engine/` (folder name predates the milestone's
current name — not worth a disruptive rename right now) — covers
`ARCHITECTURE.md` Stage 1 (app submission) and feeds directly into Stage 6
(static structural profile): an app is submitted, gets evaluated
structurally, and produces a profile + human-readable summary ready to
hand off to Milestone 2's intake conversation.

| # | Task | Status |
|---|---|---|
| 1.1 | Intake handling (FSEvents, quarantine xattr, origin URL, file lock) | Built, tested live |
| 1.2 | Source reputation check (Google Safe Browsing) | Built; needs `INSULA_SAFE_BROWSING_API_KEY` to actually check anything — degrades to `Unknown` without it |
| 1.3 | Local signature check (Gatekeeper/codesign, incl. ad-hoc detection) | Built, tested live |
| 1.4 | File type classification (magic bytes, not just extension) | Built, tested live |
| 1.5 | Notability scoring engine (9-rule cascade) | Built, unit-tested |
| 1.6 | Entropy + notable-string analysis | Built, tested live |
| 1.7 | Filename obfuscation detection (bidi-override, double extension) | Built, tested live |
| 1.8 | Archive pre-inspection, hashing, ad-hoc signature depth | Built, tested live (including a real bug found and fixed — see below) |

Implemented in `/Users/nsoz/Developer/Insula/src/` (Rust). `cargo test`:
39 passing tests. `cargo clippy`: clean. Live-tested against real
simulated inputs (benign file, disguised Mach-O, RTL-override filename,
double extension, pipe-to-shell script, zip with a nested `.app`, tar/tar.gz
with a nested high-notability entry, high-entropy random-content file,
ad-hoc-signed binary, a symlink disguised as a normal file, and duplicate
filenames landing on separate occasions) — every scenario produced the
expected tier/signal/profile. The file-locking guarantee (task 1.1) was
verified directly: a locked test file returned "permission denied" when
execution was attempted.

**Two real bugs were found and fixed during testing, both with regression
tests locking in the fix:**

1. **Archive pre-inspection missed nested app bundles.** The original
   check looked at the *whole* zip/tar entry path's trailing extension,
   which misses `Foo.app/Contents/MacOS/Foo` — `.app` sits on an
   intermediate directory component, not the final segment. Fixed to
   check every path component's own extension
   (`archive_inspection_catches_app_bundle_nested_inside_a_zip`,
   `tar_inspection_catches_app_bundle_nested_inside_a_tar`).
2. **A correctness bug in the locking mechanism itself.** `lock_file` used
   `std::fs::metadata`/`set_permissions`, both of which follow symlinks by
   default — an input that was actually a symlink pointing at an arbitrary
   real file elsewhere on disk would have had *that file's* permissions
   silently modified by Insula itself, not the symlink. Fixed by detecting
   symlinks via `symlink_metadata` (which doesn't follow the link) and
   skipping permission-stripping entirely for them — renaming a symlink is
   still safe, it never touches the target. Verified live: a symlink
   pointing at a real, executable test file kept that file's permissions
   completely unchanged before and after processing. Symlinks are now also
   treated as maximally notable on their own (Rule 0 in the scoring engine,
   ranked above even the reputation check) — a genuine browser/app download
   is never a symlink.
3. **(Also functional) A duplicate-filename bug found alongside the
   above:** the original per-path `seen: HashSet` deduplication never
   forgot a path once processed, so a *second* file later reusing an
   earlier filename (e.g. two separate `report.txt` downloads) was
   silently ignored forever — confirmed live before the fix. Removed the
   permanent set; the physical existence check already provides real
   duplicate-event suppression once a file's been moved to holding, and
   `unique_holding_path` (task 1.1) now avoids collisions in the holding
   directory itself when two different files share a name.

**New in this pass:** tar/tar.gz archive pre-inspection (previously
zip-only), a much larger notable-string pattern list, ad-hoc-signature
detection, and head-*and*-tail sampling for large files in entropy/string
analysis (a head-only cap is a known evasion target — padding content past
a scanner's size limit).

**Hard invariant across every task in this milestone:** the submitted file
is never executed, opened, or invoked on the host. Every check reads
bytes, metadata, or structure only — actually running it is the VM's job,
starting at Milestone 3.

## Milestone 2 — Consent & Launch (in progress, no task breakdown yet)

No `milestone-2-consent-launch/` task files exist — this milestone's CLI
got built through direct iterative design with the user instead of the
design-doc-first process Milestone 1 used. See `STATUS.md` for full detail.
Short version of what exists now:

- `src/bin/insula_cli.rs` — CLI entry point. Self-relaunches inside Kitty on
  startup if Kitty is installed but isn't already the current terminal, so
  the same experience is available regardless of which terminal was open
  when it was launched (no install without consent if Kitty isn't present —
  falls back gracefully).
- `src/cli/kitty_mark.rs` — a hand-rolled Kitty graphics-protocol
  implementation for the background watermark: transmits the mark image
  once, then only sends cheap placement-resize updates on every terminal
  resize, giving true real-time resize tracking. (`ratatui_image`'s own
  Kitty support re-transmits the full image on every resize and can't do
  this — see the module's doc comment for why this had to be hand-rolled.)
- `src/cli/background.rs` — fallback watermark renderer for non-Kitty
  terminals (iTerm2, Sixel, Terminal.app's half-block approximation):
  settle-then-redraw, not real-time, since those protocols have no
  equivalent of Kitty's cheap placement-resize mechanism.
- `src/cli/command_line.rs` — a classic, borderless, terminal-flow command
  input reserved below the watermark: dynamically sized (grows with typed
  history, capped at 70% of the frame), keyboard scroll (`Up`/`Down`,
  `PageUp`/`PageDown`) and mouse-wheel scroll through history, snapping
  back to the live bottom on new input.

**Still missing:** Stage 1 (app submission — the user handing Insula an
app through the CLI; mechanics still open, `OPEN_QUESTIONS.md` §8) and
Stage 2 (the conversational intake — what do you want to know, manual or
Claude-driven exploration) don't exist yet. The CLI work so far is the
visual/interactive shell these stages will be built on top of, not any of
the stages themselves.

## Not yet designed

Milestones 3, 5 (Isolated Delivery, Live CLI Streaming) — no task
breakdown exists yet.

Milestones 4, 6 (Observation Mode, Final Report) — no task breakdown
exists yet. Milestone 4 covers the manual/Claude-driven exploration modes
(Stage 5); Milestone 6 covers the full evidence report + the conversational
Q&A layer + the confidence-tiered inferred structure sketch (Stage 8), not
just a flat summary.
