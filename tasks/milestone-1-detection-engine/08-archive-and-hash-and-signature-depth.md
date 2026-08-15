# Task 1.8 — Archive pre-inspection, hashing, and signature depth

**Goal:** three more static-only techniques rounding out the local
detection surface. **Hard invariant for every technique in Milestone 1:
the downloaded file is never executed, opened, or invoked — every check
reads bytes, metadata, or structure only.**

## 1. Archive content pre-inspection (without extracting)

Task 1.4 currently scores `.zip`/archive files as flat `Medium` risk
without looking inside. A zip's **central directory listing** can be read
without extracting anything — if it contains an `.app`, `.command`, a
Mach-O binary, or another high-risk type, the archive's tier is upgraded
to `High` and that inner filename is recorded in the reason text. Uses the
`zip` crate (pure Rust, reads the listing only — no shell-out, no
extraction to disk).

## 2. SHA-256 hashing

Every evaluated file gets a SHA-256 computed and attached to its
assessment. Not a detection signal by itself yet (no local
known-bad-hash list exists to check against, and `VirusTotal` was
deliberately deferred) — but it's the connective tissue between Milestone
1's local checks and any hash-based service added later, and it's the
right thing for a report to cite regardless. Purely static, reads bytes
only.

## 3. Ad-hoc vs. proper Developer ID signature distinction

Task 1.3's `SignatureTier::SignedNotNotarized` currently doesn't
distinguish *how* something is signed. `codesign -dv` reports whether a
signature is **ad-hoc** (`Signature=adhoc` in its output) — a signature
type anyone can generate locally in seconds, proving nothing about the
publisher's identity, versus a real Developer ID signature backed by
Apple's own certificate chain. A malicious binary signed ad-hoc just to
get past a naive "is it signed at all" check is a known evasion pattern —
this refines the existing signature check output, no new external call.

## Research note — techniques considered and deferred, not dismissed

- **Trailing-data-after-EOF steganography** (a payload appended after a
  carrier file's real end-of-data marker, e.g. past a JPEG's `0xFFD9`) is
  a real technique, but implementing it properly needs per-format parsing
  for each carrier format it could hide behind — out of scope for today,
  worth a dedicated task later.
- **Full Mach-O load-command parsing** (entry points, linked-framework
  table, entitlements) would be more precise than string-scanning for
  sensitive framework references, but is a meaningfully larger parser to
  write correctly. The string-scan approach in task 1.6 already catches
  the common case (a suspicious binary's linked-framework paths and
  API-abuse-relevant symbol names show up as plain strings in the binary
  regardless) at a fraction of the implementation cost.
