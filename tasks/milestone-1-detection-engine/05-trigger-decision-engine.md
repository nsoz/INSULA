# Task 1.5 — Trigger decision engine

**Goal:** combine tasks 1.2–1.4's signals into the single binary decision
`ARCHITECTURE.md` Stage 1 calls for — trigger the Stage 2 OS notification,
or let the download pass silently. Deliberately a rule set, not a tunable
score, per Stage 1's original design ("the rule itself stays simple — not
a tunable severity threshold the user has to reason about").

## Decision rule (first draft — expected to be refined once tested)

1. Safe Browsing flags `malicious` → **trigger**, regardless of anything
   else. Verified external threat intel outranks local heuristics.
2. Else if `extension_mismatch` is true → **trigger**, regardless of tier.
   A file lying about its own type is a strong signal on its own.
3. Else if file-type tier is `high` **and** signature is `unsigned` or
   `gatekeeper-rejected` → **trigger**.
4. Else if file-type tier is `medium` or `high` **and** signature check
   returned `not applicable` (a format Gatekeeper doesn't evaluate, e.g. a
   loose script) → **trigger** — conservative default for risk-bearing
   types we can't verify locally.
5. Otherwise → **no trigger**, download proceeds untouched.

## Output

- Boolean trigger decision.
- A human-readable reason string naming which rule(s) fired — this is what
  actually populates the Stage 2 OS notification's text (e.g. *"this file
  is unsigned and a high-risk type — run a check?"*), not just an opaque
  yes/no.

## Input

The outputs of tasks 1.2 (URL reputation), 1.3 (local signature), and 1.4
(file type risk).
