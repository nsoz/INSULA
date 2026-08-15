# Task 1.6 — Entropy and suspicious string analysis

**Goal:** extend task 1.4's static analysis with two more well-precedented,
purely local techniques — no new external dependency, only a new Rust
crate (`zip`, added in task 1.8) and hand-written math/parsing.

## Research basis

- **Shannon entropy** — classic static-analysis heuristic used across the
  AV/EDR industry (tools like `pescan`, Detect It Easy, most commercial
  engines' static tier). Packed or encrypted payloads look close to random
  noise (entropy near 8.0 bits/byte); ordinary compiled code and text sit
  meaningfully lower. Packers (UPX and malware-specific crypters) exist
  specifically to defeat signature scanning, and high entropy is the
  standard tell for "something is hiding its real content here."
- **Suspicious string scanning** — a lightweight version of what YARA rules
  do in production tools: extract printable strings from a file and check
  them against known-suspicious patterns (shell pipe-to-execute idioms
  like `curl | bash` / `curl | sh`, `osascript` invocations, base64 blobs
  large enough to be an embedded payload rather than incidental data,
  common persistence-mechanism paths like `LaunchDaemons`/`LaunchAgents`,
  raw IP-literal URLs). Full YARA (the `yara` crate) requires the system
  YARA C library as an install-time dependency — a hand-written pattern
  scanner avoids that for v1, at the cost of being far less comprehensive
  than real YARA rulesets.

## Design decision — entropy is a *contributory* signal, not standalone

Legitimately compressed installers (`.dmg`, `.zip`) are also high-entropy —
scoring every archive as suspicious purely on entropy would be constant
false positives. Entropy is only checked (and only counts toward a trigger)
on files already in the **high-risk tier** from task 1.4 — i.e. "this is
already an executable-shaped file, and on top of that, its content looks
packed/encrypted" is the actual signal, not entropy in isolation.

## Output

Extends `FileTypeAssessment` (task 1.4's output) with:

- `entropy: Option<f64>` — Shannon entropy in bits/byte.
- `high_entropy: bool` — true if entropy exceeds a threshold (~7.5) *and*
  the file is already high-risk tier.
- `suspicious_strings: Vec<String>` — any matched patterns, kept for the
  eventual report (task 1.5's reason text and, later, the CLI report).
