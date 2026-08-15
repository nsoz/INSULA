# Insula — Roadmap

This consolidates the v1/v2 splits made throughout early design discussion
into one place. The pattern repeats across several unrelated parts of the
system: prove the core loop works end to end in a deliberately narrowed
form first, then spend effort on realism/depth whose value only shows up
once that loop already runs. Each item below traces back to where it was
actually decided.

---

## v1 — prove the core loop works

**Goal:** submission → intake → VM → exploration → observation → scoring →
AI synthesis → report, running end to end — even narrowed — before
investing further in work whose payoff depends on the loop already
existing.

- **VM tooling & guest OS (host-OS-adaptive):** at install time, Insula
  detects the host OS and picks a matching backend *and* a matching guest —
  the guest mirrors the host rather than defaulting to whichever OS is
  lightest, because a mismatched guest produces misleadingly incomplete
  results for apps targeting the host's actual OS.
  - **Linux host** → KVM/Firecracker-style microVM tooling, with a minimal
    Linux guest (e.g. Alpine). Baseline size in the 1–2GB range.
  - **macOS host** → Apple `Virtualization.framework`, with a macOS guest
    (not Linux). Heavier than the Linux path — there's no "Alpine of
    macOS" — likely in the same order of magnitude as the earlier Windows
    estimate (tens of GB) rather than a couple GB, but still the only
    choice that produces meaningful results for macOS-targeted apps.
  - **Windows**, as either host or guest, is out of scope for v1 entirely.
  The real cost of this whole v1 scope: without Windows support, apps built
  only for Windows can't be analyzed on either host path yet.
- **Decoy environment:** a plain, classic VM — no structural mimicry of the
  real device, can be entirely empty inside. An app that specifically
  reaches for decoy accounts/apps to change its behavior won't be caught in
  v1, but the observation, scoring, and reporting engine is fully exercised
  without it — decoy realism and pipeline correctness are separable
  concerns.
- **Observation mechanism:** an in-guest lightweight sensor (e.g. eBPF,
  given a Linux guest) streaming events out to the host observer — not
  full hypervisor-level introspection.
- **Delayed-trigger evasion:** treat the act of querying elapsed uptime or
  self-run-count as a signal on its own. No VM clock acceleration yet.
- **Platform:** personal computer only.
- **Entry point & exploration mode:** the user hands Insula an app directly
  via the CLI (`ARCHITECTURE.md` Stage 1), followed immediately by the
  Stage 2 conversational intake that asks what the user actually wants to
  know. Exploration itself is **manual only** in v1 — the user drives the
  app inside the VM themselves. Claude-driven exploration (VM input
  injection guided by the guest's accessibility tree) is real new
  infrastructure on top of what proving the core loop needs, so it's
  deferred to v2.
- **AI synthesis scope:** the plain-language narrative, color-coded flags,
  and conversational Q&A grounded in evidence all ship in v1 — this is a
  synthesis/prompting layer over evidence the pipeline is already
  collecting, not new infrastructure. The confidence-tiered **inferred
  structure sketch** (`PROJECT.md`'s "Core concept") ships in v1 too, for
  the same reason — how the "verified narrow mechanism" tier actually gets
  validated is still open, see `OPEN_QUESTIONS.md` §7.

## v2 — close the gap to the original vision

- **VM tooling / guest OS:** add Windows support (as both a possible host
  and a guest, likely via a heavier, traditional VM rather than a
  lightweight framework) alongside the existing Linux-host and macOS-host
  paths, once the core loop is proven — this is what brings
  Windows-targeted apps into scope.
- **Decoy environment:** build the full "populated but empty" structural
  mimic described in `PROJECT.md` — same installed-application and folder
  structure as the real device, no real data anywhere. The generation
  method is still open — see `OPEN_QUESTIONS.md` §1.
- **Observation mechanism:** move from an in-guest sensor to true
  out-of-guest, hypervisor-level introspection (VMI), so the observer is
  unobservable from inside the guest — the commitment `PROJECT.md` §4
  actually makes.
- **Delayed-trigger evasion:** add VM clock acceleration as a second
  mitigation layer on top of query-as-signal — see `OPEN_QUESTIONS.md` §2
  for the open sub-questions on how aggressive that can be.
- **Claude-driven exploration:** active agentic exploration of the app
  inside the VM — reading the guest's accessibility tree to know what's
  clickable, injecting synthetic input to navigate menus/settings/features,
  falling back to screenshot-based vision where a usable accessibility tree
  isn't exposed — offered as a user-chosen alternative to manual
  exploration once the core loop is proven. See `OPEN_QUESTIONS.md` §6 for
  what's still undecided about the injection/isolation mechanics.

## Future — not yet committed

- **A server/infrastructure variant.** Same core reasoning (isolate,
  observe, understand) applied to a fundamentally different trigger model
  (CI/CD artifacts, package installs, uploads — not a user directly
  submitting an app), a different consent model (automated or asynchronous
  review, not an interactive CLI session with a human), and a different
  decoy concept (honeytokens/canary credentials, not personal-account
  mimicry). Deliberately kept separate from v1/v2: this is a different
  product shape aimed at a different user, not a deeper version of the
  desktop tool, and shouldn't shape decisions made for it.
