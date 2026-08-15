# Task 1.7 — Filename obfuscation detection

**Goal:** catch classic filename-based social-engineering tricks that
extension/content mismatch (task 1.4) doesn't cover on its own — these are
about the *name*, not the *content*.

## Research basis

- **Right-to-left override (RTL/Unicode bidi trick)** — a well-documented,
  real technique (used by malware families going back to Sality/others in
  email-borne campaigns): inserting U+202E (RIGHT-TO-LEFT OVERRIDE) into a
  filename reverses how the rest of the name displays. A file physically
  named `...fdp.exe` with a U+202E inserted before `fdp.exe` *displays* as
  `...exe.pdf` — the user sees what looks like a PDF, but the real
  extension (and what actually runs) is `.exe`. Detectable by scanning for
  the bidi-override codepoint (and its relatives — U+202A-U+202E, U+2066-
  U+2069) anywhere in the filename; a legitimate file has no reason to
  contain one.
- **Double extensions** — `invoice.pdf.app`, `photo.jpg.command` — the
  visible/leading extension is the bait, the trailing one is what the OS
  actually treats the file as. Detectable by checking whether the
  second-to-last dot-separated segment is itself a plausible, different
  file extension.

## Output

A new field on the download-event pipeline (surfaced via task 1.4's
`FileTypeAssessment`, since it's evaluated alongside the rest of the type
analysis): `filename_obfuscation: Option<String>` — `None`, or a
human-readable description of what was found (fed into task 1.5's reason
text).
