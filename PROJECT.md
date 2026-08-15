# Insula

**A consent-gated, VM-isolated dynamic analysis tool: hand it any app, and
it runs that app in complete isolation, observes everything it actually
does, and lets you understand it — through evidence, conversation, and an
honestly-labeled inferred picture of how it works — grounded entirely in
what was actually observed, never guessed or invented.**

> *Insula* — Latin for "island." The core metaphor: whatever app Insula is
> asked to look at is run in isolation, on its own island, observed in
> complete detail, before anything about it is explained or trusted.

## The problem

Understanding what a piece of software actually does — not what its
marketing page or a single install-time permission prompt claims, but what
it genuinely does when it runs — is hard to get an honest answer to. Static
inspection alone (reading strings, checking a signature) tells you very
little about behavior. Reading a marketing page tells you nothing
verifiable at all. The only honest way to know is to watch the thing run,
in detail, in an environment where doing so is safe regardless of what it
turns out to do.

This is a real need whether the motivation is curiosity ("what does this
app actually access on my machine?"), professional reverse engineering, or
learning how real dynamic analysis works by building a working version of
it. Insula exists to make that observation genuinely usable: not a wall of
raw syscalls the user has to interpret alone, but a system they can talk
to about what it found.

## Core concept

The user hands Insula an app they want to understand, directly through the
CLI. Before anything runs, Insula talks to the user first: what do you
actually want to know? If the goal is just curiosity about the app's
on-device behavior, Insula explains that the user has full manual control
to go explore every part of the app themselves once it's running. Either
way, the user gets a real choice for how the exploration itself happens:

- **Manual** — the user interacts with the app inside the VM directly.
- **Claude-driven** — Claude actively explores the app: reading the guest's
  accessibility tree to know what's on screen and clickable, injecting
  synthetic input to navigate menus/settings/features, falling back to
  screenshots where an app doesn't expose a usable accessibility tree —
  exercising far more of the app's behavior than a single manual session
  typically would.

Observation itself works the same way regardless of exploration mode:
continuous, indiscriminate, from the VM's own boot, from entirely outside
the guest. Afterward, Insula presents a report through the CLI — a
timestamped behavioral log, a static structural profile (imports, strings,
signing info, entropy), evasion/anomaly signals, known-pattern fingerprint
matches — and the user can interrogate that evidence conversationally,
asking follow-up questions grounded in what was actually recorded.

Optionally, with explicit consent, Insula can go one step further and
synthesize an **inferred structure sketch** — a plain-language or
pseudocode-shaped account of how the app appears to work. This is always
labeled honestly as *inferred from observed behavior*, never as extracted
or decompiled source, and its confidence is shown per-claim: a narrow
mechanism that was actually validated (e.g. a specific network protocol or
transform, confirmed by testing it against varied real inputs) is marked
differently from a broader narrative sketch that reflects only the paths
actually exercised and may be missing real logic entirely. This exists
because a finite observation session — however thorough — only ever
samples some of a program's possible execution paths; branches never
triggered stay completely invisible no matter how long you watch. Insula
is honest about that limit rather than papering over it with confident
guesses.

The system is built around five ideas, each addressing a specific way this
kind of investigation normally fails:

### 1. A realistic environment gets closer to true behavior

A VM that is obviously fresh and empty is itself a signal — plenty of real
software checks for "is this a real user's machine" (installed software,
existing files, account state) before behaving normally, whether to defeat
analysis specifically or just because it assumes a populated environment.
Insula's isolated VM mirrors the *structure* of the user's real device
without mirroring its *data*: the same folder names, the same installed
applications, files with the same names in the same places — but empty,
and with no accounts logged in anywhere. This gets any app under analysis
closer to behaving the way it would on a real machine, rather than the way
it behaves once it notices it's being watched.

### 2. Reconnaissance and evasion are evidence, not noise

Everything a process does inside the guest OS — checking file sizes,
listing directories, querying whether an application is installed, timing
its own execution to detect virtualization — has to pass through the
operating system as an observable syscall; none of it can be hidden from a
monitor sitting outside the guest. Two categories of behavior are treated
as first-class evidence, not just background noise:

- **Evasion checks** — a program probing for signs it's running in a VM
  (timing checks, known hypervisor artifacts, sandbox process names) is
  itself a meaningful signal about how much to trust its observed behavior
  as representative of its real-world behavior.
- **Irrelevant enumeration** — a program whose stated purpose has no
  logical connection to unrelated installed software or folders it's
  probing is exhibiting behavior worth surfacing in the report.
- **Temporal / execution-count fingerprinting** — some software delays
  acting until a specific run count or elapsed uptime, precisely to outlast
  a typical observation window. Querying either with no functional reason
  to is the same category of tell as a VM-detection check.

None of these are scored as a hard yes/no rule on their own — a single
`stat()` call isn't damning — but broad, purpose-mismatched probing is
weighted as a strong anomaly signal, and shown to the user as part of the
evidence, not silently absorbed into a single score.

### 3. Watch everything, indiscriminately

The observation layer does not pre-filter for "relevant" activity, and it
does not start watching only once the app arrives. Observation begins at
the VM's own boot, before the app ever lands inside it, and continues
without interruption through transfer, any exploration (manual or
Claude-driven), and any execution that follows — one continuous window.
Every thread the app spawns, every process it touches, every component of
the VM it interacts with within that window is logged in full. Deciding
what mattered happens at analysis time, not at capture time.

### 4. The observer must be unobservable

Monitoring happens from outside the guest OS, not through an in-guest
agent. An agent living inside the VM is, in principle, something
sufficiently sophisticated software could detect and evade. A monitor that
never runs inside the environment it's watching has no presence for
anything inside that environment to find. This applies equally to the
Claude-driven exploration mode's input-injection channel — it's a
one-directional, host-controlled path into the guest, not a process
running inside it, and carries no shared clipboard or shared folder that
anything inside the guest could use to reach back out.

### 5. The human stays in the loop, and gets to actually talk to Insula

Insula does not just hand back a flat summary at the end. It asks what the
user wants to know before running anything, lets the user choose how the
exploration itself happens, and — after the run — lets the user question
the evidence conversationally rather than just reading a static report.
The CLI is the surface for the entire journey, from the initial
conversation to the final, interrogable report.

## What the output looks like

Once a run completes, Insula produces a real evidence set, not a flat,
one-line summary:

- The full, timestamped sequence of what the app did inside the VM.
- A static structural profile: imported libraries/APIs, embedded strings,
  signing/entitlement info, section entropy (a packing/obfuscation
  indicator).
- Notable or anomalous actions visually distinguished (color-coded) from
  routine ones, including the evasion/reconnaissance signals from
  principle 2.
- Known-pattern/library fingerprint matches, where applicable.
- An AI-assisted plain-language narrative of what happened and why it's
  worth noting (via an external LLM API — the one deliberate exception to
  Insula's no-external-dependency stance).
- A conversational layer: the user can ask follow-up questions and get
  answers grounded in the actual recorded evidence, not invented.
- Optionally, with explicit consent, an **inferred structure sketch** — see
  "Core concept" above for exactly what this is and isn't, and why its
  confidence is shown per-claim rather than presented as uniform.

All of it is delivered back to the user through the same CLI, from the
initial conversation to the final, questionable report.

## What Insula is not

Keeping this explicit matters, both to keep the build scoped and to be
honest about what the system claims:

- **Not a full network traffic monitor.** Insula only acts on apps the
  user deliberately hands it — not a firewall, not a packet sniffer for
  general traffic.
- **Not a decompiler, and never claims to be one.** It never asserts that
  it has extracted or recovered an app's actual source code. Any structure
  sketch it offers is an explicitly labeled *inference from observed
  behavior*, confidence-tiered, and — for anything beyond a narrow,
  actually-verified mechanism — incomplete by construction: a finite
  observation session samples finite execution paths, and whatever wasn't
  exercised stays genuinely unknown, not silently guessed at.
- **Not capable of inspecting end-to-end encrypted traffic** (e.g.
  messaging apps). That is a cryptographic impossibility, not a missing
  feature.
- **Not a finished commercial product.** This is a portfolio-stage
  foundation project, built to demonstrate the architecture and the
  reasoning behind it working end to end — not to run production traffic or
  handle real risk at scale.

## Safety guarantees

Because this system's job is running software whose behavior is genuinely
unknown until observed, its own containment has to be trustworthy:

- The isolated VM never has access to the user's real data — only a
  structural mimic of it (see principle 1).
- The VM's network access is isolated from the user's real network; only
  the observation stream (logs) crosses that boundary, not general traffic.
- The Claude-driven exploration mode's input-injection channel is
  one-directional and host-controlled — no shared clipboard, no shared
  folder, nothing the guest could use to reach back out through it.
- Every run starts from a clean snapshot and the VM is reverted afterward —
  nothing persists between runs.
- No real credentials or account sessions exist anywhere in the isolated
  environment, ever.

## Why this matters as a project

This isn't an attempt to out-build Ghidra, IDA, or Any.Run. It's a
demonstration that the reasoning behind real dynamic analysis tooling —
realistic-environment observation, evasion-as-signal, out-of-guest
monitoring, agentic exploration grounded in accessibility tooling, and
honest, confidence-labeled AI synthesis instead of overclaimed
decompilation — can be understood deeply enough to design and build a
working version of it, end to end, alone.
