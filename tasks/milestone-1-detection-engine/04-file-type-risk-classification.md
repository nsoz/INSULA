# Task 1.4 — File type risk classification

**Goal:** classify what the file actually is, and how risky that category
is — using real content, not just the claimed extension.

## Mechanism

- Read the file's magic bytes / header signature to determine its **true**
  type, independent of the extension it arrived with. A file claiming to
  be `.jpg` that's actually a Mach-O executable is itself a strong signal
  — flagged explicitly as an extension/content mismatch, not silently
  folded into a tier.
- Classify the true type into a tier:
  - **High** — app bundles, `.pkg`, `.command`, Mach-O/ELF executables,
    shell scripts, macro-enabled Office documents, disk images (`.dmg`).
  - **Medium** — archives (`.zip`, `.tar.gz`) that could contain any of
    the above once extracted, without inspecting contents yet.
  - **Low** — plain media, text, PDFs without macros. (Noted, not solved,
    limitation: PDFs can still carry exploits — "low" here means lower
    tier, not zero risk.)

## Input / Output

- **Input:** file path from task 1.1.
- **Output:** risk tier (`high`/`medium`/`low`), detected true file type,
  and an `extension_mismatch` flag (bool).
