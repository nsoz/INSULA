# Task 1.2 — URL reputation check

**Goal:** check the download's origin URL against a real, continuously
updated threat database before deciding whether to trigger.

## Mechanism

- **Google Safe Browsing API v4** (Lookup API, `threatMatches:find`) — send
  the origin URL, get back any matching threat types (`MALWARE`,
  `SOCIAL_ENGINEERING`, `UNWANTED_SOFTWARE`,
  `POTENTIALLY_HARMFUL_APPLICATION`), or nothing if clean.
- This is the one deliberate second external dependency beyond the AI API
  already accepted in `PROJECT.md` — justified the same way: no local
  heuristic can replace a continuously updated, crowd-sourced threat feed,
  and Safe Browsing is free and built specifically for this purpose.
- Cache verdicts locally (keyed by URL, short TTL) to avoid redundant calls
  and stay within the free tier's rate limits.

## Input / Output

- **Input:** origin URL from task 1.1's download-event record.
- **Output:** a verdict — `malicious` (with the matched threat type),
  `clean`, or `unknown` (no URL was available to check, e.g. a
  non-browser download task 1.1 couldn't resolve an origin for).
  `unknown` contributes no signal either way — it doesn't block the
  pipeline, task 1.5 just proceeds without this input.

## Open question

Where the Safe Browsing API key is stored/managed — needs a decision before
implementation, not solved here.
