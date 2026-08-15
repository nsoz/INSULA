# Task 1.1 — Download detection

**Goal:** notice a new download has landed, before anything else in the
milestone can run — this is the event source everything downstream depends
on.

## Mechanism

- Watch common download locations (`~/Downloads` first; expand later)
  using **FSEvents**.
- Don't act on file creation — browsers write to a temp name (`.crdownload`,
  `.download`, etc.) during transfer and rename to the final filename on
  completion. Watch for that **rename**, since it's both "download finished"
  and "file is now stable" in one signal.
- On rename, check for the `com.apple.quarantine` extended attribute —
  macOS sets this automatically for any download from a quarantine-aware
  app (Safari, Chrome, Mail, Slack, etc.). No xattr → not treated as a
  genuine download event (see limitation below).
- Extract the origin URL from the quarantine metadata (the xattr encodes an
  event UUID that resolves to origin URL + referrer + source app in
  macOS's own quarantine events store). Exact API/schema needs verifying
  against the current macOS version during implementation — the mechanism
  is known to exist, the precise lookup path isn't pinned down yet.

## Locking the file — the actual guarantee, not a speed race

Racing the rest of the pipeline against how fast a user might click to open
the file isn't a real guarantee — task 1.2's network call (Safe Browsing)
alone can spike well past a human's reaction time on a bad connection. So
the moment this task confirms a genuine download event (step above), before
handing off to tasks 1.2–1.4, it immediately makes the file physically
inert: strip execute permission and, for app bundles/documents openable
without execute (e.g. via double-click through Launch Services rather than
a shell), move the file into a holding location outside its expected
path so nothing can resolve it by its original name yet.

This lock is not released by this task. It's released downstream, once the
full chain resolves — task 1.5 decides not to trigger, the user declines
the Stage 2 notification, or a Stage 9 verdict comes back clean. Until one
of those happens, the file simply cannot be opened, regardless of how fast
or slow the analysis is. That release logic belongs to whichever later
milestone owns Stage 2/Stage 9 — noted here as a dependency, not designed
here.

## Runtime model

Runs as a persistent background service (macOS `LaunchAgent`), independent
of the CLI — per `ARCHITECTURE.md`, the CLI only opens once the user
accepts the Stage 2 notification, so detection has to keep running without
it.

## Output

A structured download-event record, passed to tasks 1.2–1.4:

- file path
- filename + claimed extension
- quarantine flag confirmed (bool)
- origin URL (if resolved)
- timestamp
- source app/agent

## Known limitation

Downloads via `curl`/`wget` or other non-quarantine-aware tools don't set
the xattr and are invisible to this mechanism. Accepted as a v1 gap, not
solved here.
