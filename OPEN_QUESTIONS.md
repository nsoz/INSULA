# Insula — Open Questions

Unresolved design and engineering questions, surfaced during early design
discussion. These are meant to be closed out in `ARCHITECTURE.md` or during
implementation planning — this file just makes sure nothing raised gets lost
before then.

---

## 1. How do we generate a high-fidelity decoy VM?

`PROJECT.md` establishes that the isolated VM needs to mirror the real
device's *structure* (folder names, installed applications, file paths)
without mirroring its *data* — populated enough that an app whose behavior
depends on a real-looking environment still behaves normally, empty enough
that nothing real is ever at risk. The principle is settled; the method to
actually build it is not.

Open sub-questions:

- What's the actual generation process — scanning the real device's
  directory tree and installed-application list locally (this scan and its
  output never leave the device) and producing a matching empty/placeholder
  skeleton? What does that scanner need to detect, and how reliably?
- Some applications may need more than an empty folder with the right name
  to look "installed" convincingly (expected config files, registry entries,
  a plausible-looking local cache) — does this require per-application
  templates for common software (browsers, Steam, VS Code, Discord, etc.),
  and if so, how many is "enough" before the decoy is credible?
- How deep does the mimicry need to go before it's convincing, versus where
  effort stops paying off? `PROJECT.md` already flags that file size,
  timestamp, and content-hash checks by a sufficiently thorough app are a
  known limitation of a v1 "same name, empty content" approach — this needs
  a concrete answer for what v1 actually ships with, and what's deferred.

## 2. Countering delayed-trigger evasion

Reference: `PROJECT.md` → "Reconnaissance is evidence, not noise" → temporal
/ execution-count fingerprinting.

Two mitigations were proposed, neither fully solving the problem alone:

- Treating the *act of querying* elapsed time or run count as a signal on
  its own, independent of whether a delayed payload ever fires within the
  observation window.
- Accelerating the VM's perceived system clock so a time-gated payload can
  be provoked without waiting in real time.

Open sub-questions:

- Do we implement both, or start with the cheaper one (query-as-signal) and
  treat clock acceleration as a v2 addition?
- How aggressive can clock acceleration be before it becomes its own
  detectable artifact (some evasive software checks for implausible clock
  jumps as *its own* evasion-detection signal)?

## 3. VM / hypervisor tooling

Resolved for v1, per `ROADMAP.md`: host-OS-adaptive, decided once at
install time (`ARCHITECTURE.md` Stage 0). A Linux host uses
KVM/Firecracker-style microVM tooling; a macOS host uses Apple's native
`Virtualization.framework`. Windows, as either host or guest, is out of
scope for v1 entirely. Backend selection is a pluggable decision point, not
hardcoded to one host, so Windows can be added in v2 without redesigning
Stage 0.

Still open:

- Which concrete tool sits on top of `Virtualization.framework` on a macOS
  host — Lima, Colima, or a purpose-built tool talking to the framework
  directly?
- Same question on the Linux-host side — Firecracker directly, or a
  higher-level manager on top of it?
- What does adding Windows as a third host-OS branch in v2 actually
  require — is the pluggable interface from Stage 0 sufficient, or does it
  need to change once there's a real third implementation to generalize
  from?

## 4. Guest OS for the isolated VM

Resolved for v1, per `ROADMAP.md`: the guest **mirrors the host OS** rather
than defaulting to whatever's lightest — a Linux host gets a minimal Linux
guest (e.g. Alpine, ~1–2GB), a macOS host gets a macOS guest (heavier,
likely tens of GB — no equivalent of a minimal "Alpine of macOS" exists).
This was a deliberate reversal of an earlier draft of this decision, which
had picked a Linux guest universally for its disk savings — that would have
produced misleadingly incomplete results for macOS-targeted apps on a
macOS host, since a mismatched guest often just fails to run the app the
way its real target would.

Still open:

- How to strip down a macOS guest install to reduce its footprint — there's
  no established minimal/embedded macOS distribution to build from the way
  Alpine serves that role for Linux.
- Which Windows image to build against once Windows support is added in
  v2 — a freely-available, legally redistributable one (e.g. Microsoft's
  evaluation/developer VM images) — and what licensing constraints that
  carries for a public portfolio repo.

## 5. Core implementation language(s)

Leaning toward **Rust** for the core system (VM orchestration, the
out-of-guest observation pipeline) — consistent with the reasoning already
used for Morphological Tokenizer, and a good fit for something doing a lot
of low-level, correctness-sensitive system work. The log-analysis /
AI-API-calling layer may end up as a separate, lighter component (Python or
TypeScript) rather than forcing everything into one language.

Not yet decided.

## 6. Claude-driven VM exploration mechanics

Reference: `PROJECT.md` → "Core concept" (the user's choice between manual
and Claude-driven exploration) and `ARCHITECTURE.md` Stage 2/5. The
capability is decided (deferred to v2, per `ROADMAP.md`); the mechanics
that make it real are not.

Open sub-questions:

- How does synthetic input injection actually work per host backend —
  what does `Virtualization.framework` expose for pointer/keyboard
  injection on macOS, and what's the equivalent on the Linux/KVM path?
- How is the guest's accessibility tree read remotely, per guest OS (macOS
  `AXUIElement`, Linux AT-SPI) — does this require an in-guest bridge
  process, which would be in tension with `PROJECT.md` principle 4 (the
  observer must be unobservable), or can it be read from the host side
  through the hypervisor?
- What's the fallback strategy when an app doesn't expose a usable
  accessibility tree at all — pure screenshot + vision, and if so, how
  reliable does click-target identification need to be before it's worth
  shipping?
- `PROJECT.md`'s safety guarantees promise the injection channel is
  "one-directional and host-controlled... nothing the guest could use to
  reach back out through it." Is that enforced at the VM-tooling level (a
  real architectural guarantee), or is it currently just an intention that
  needs a concrete mechanism to actually hold?

## 7. Validating the inferred structure sketch's confidence tiers

Reference: `PROJECT.md` → "Core concept" (the inferred structure sketch,
and the distinction between a "verified narrow mechanism" and a broader
unverified narrative sketch).

The validation method described during design discussion for the
"verified" tier is differential testing — varying inputs, observing how
outputs change, and confirming a hypothesized mechanism (e.g. a specific
protocol or transform) actually holds. The concrete mechanism isn't
designed yet:

- What triggers an attempt at this — does Insula decide on its own that a
  mechanism looks narrow/tractable enough to try to verify, or does the
  user have to ask for it explicitly?
- What counts as "enough" confirming tests before something is labeled
  verified rather than a guess?
- How is the tier distinction actually surfaced in the CLI report — visual
  styling, an explicit confidence percentage, plain-language framing?

## 8. How does app submission actually work at the CLI level?

Reference: `ARCHITECTURE.md` Stage 1. The user "hands Insula an app
directly through the CLI" — but the actual interaction isn't designed: a
file path passed as a CLI argument, an interactive file picker inside the
running CLI, drag-and-drop into the terminal (some terminals support this),
or something else. Not yet decided.
