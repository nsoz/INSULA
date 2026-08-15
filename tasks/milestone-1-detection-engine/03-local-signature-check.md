# Task 1.3 — Local signature check

**Goal:** ask macOS's own Gatekeeper what it already thinks of this file —
zero network calls, zero new external dependency, since it's the OS's own
local decision.

## Mechanism

- Query Gatekeeper's verdict via `spctl --assess --type execute <path>`
  (shell-out — simplest to build) or the Security framework's
  `SecStaticCodeCheckValidity` / `SecAssessment` APIs directly (more
  correct, more implementation work). Which one to start with is left open
  for implementation time — shelling out is the pragmatic v1 default.
- Also read the code signature identity (`codesign -dv <path>`) to
  distinguish tiers, not just pass/fail.

## Output

A signature tier:

- **Notarized / trusted developer ID** — Apple-verified.
- **Signed, not notarized** — has a signature but hasn't passed Apple's
  notarization scan.
- **Unsigned.**
- **Gatekeeper-rejected** — actively blocked by current policy.
- **Not applicable** — the file isn't an executable/app bundle type this
  check means anything for (images, text, plain documents).

## Input

File path from task 1.1.
