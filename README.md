# Insula

*Insula* — Latin for "island."

A consent-gated, VM-isolated dynamic analysis tool for macOS. Hand it any
app, and it runs that app in a disposable virtual machine, observes
everything it actually does at the system level, and gives you a report
you can interrogate — grounded entirely in what was observed, never
guessed or invented.

> **⚠️ This repo ships `insula-test-payloads/EncryptApp` and
> `insula-test-payloads/DecryptApp` — a real, working ransomware-style
> file-encryption / file-decryption pair, included on purpose as a test
> sample for Insula. They are not malware left in by accident and not a
> toy simulation. They ship with execute permission stripped as a safety
> default. Read the *Test payloads* section below before touching them.**

<!--
TODO: replace with a real terminal screenshot or GIF of insula_cli running
— the live-resizable Kitty-graphics watermark plus the onboarding flow is
the most immediately convincing thing about this project and belongs here,
above the fold.
-->
<p align="center">
  <img src="assets/insula_mark.png" alt="Insula" width="480">
</p>

## Why

Understanding what a piece of software actually does isn't something you
can get from a marketing page or a permission prompt. The only honest way
is to watch it run, in an environment where doing so is safe regardless of
what it turns out to do. Insula exists to make that observation usable: a
real behavioral report instead of a wall of raw syscalls, and a
conversation instead of a static verdict.

Insula is a reverse-engineering / dynamic-analysis tool, not a
security-verdict product — there's no "safe/unsafe" judgment, no
interception, no block decision anywhere in the design.

## Notable engineering

The parts of this project that were genuinely hard, not just plumbing:

- **Kernel-level observation via Apple's EndpointSecurity framework.**
  Insula's sensor is a real ES client — NOTIFY subscriptions for
  exec/fork/exit and file create/rename/unlink, with `es_message_t`
  bindings generated straight from Apple's headers via `bindgen` rather
  than hand-transcribed. Getting a third-party ES client running at all
  requires SIP disabled and a signed entitlement — there's no way around
  either.
- **True VM isolation, not a sandboxed subprocess.** Every analysis runs
  in a freshly cloned, uniquely-named, genuinely single-use macOS VM
  (via [Tart](https://tart.run)), provisioned once as a golden image and
  cloned per run. Verifying and fixing "disposable" clones that weren't
  actually disposable meant mounting a *stopped* VM's raw disk image from
  the host side and editing it directly.
- **A real, live-diagnosed race condition.** The unattended exploration
  mode launches the target app the instant it appears in the guest's
  shared directory — but for a large binary still being copied over
  VirtioFS, that instant can come before the copy is complete. The kernel
  killed the truncated process outright (`load code signature error 2`,
  caught live from the unified log, not guessed at), fixed by waiting for
  the file size to stabilize before exec'ing it.
- **Event-tree filtering.** The sensor observes every process on the
  system, indiscriminately, from boot. Isolating just the submitted app's
  own behavior means transitively tracking its process tree from a single
  matching `exec` event through every subsequent `fork`, at any depth —
  so the report reflects only what the target app did, not boot noise.
- **Terminal-native image rendering.** The background watermark uses the
  Kitty graphics protocol for real-time, live-resizable rendering — full
  fidelity in Kitty-protocol terminals (Kitty, Ghostty, WezTerm), with a
  graceful re-encoded fallback everywhere else.

## How it works

1. **Consent & launch** — you hand Insula a path to an app; it asks what
   you actually want to know before doing anything.
2. **Isolated delivery** — the app is copied into a freshly cloned,
   uniquely-named, single-use macOS VM (via [Tart](https://tart.run)) and
   torn down afterward. Nothing runs on your real machine.
3. **Observation** — an EndpointSecurity-based sensor inside the guest
   watches exec/fork/exit and file create/rename/unlink activity from
   boot, for every process on the system, and streams it out over a
   read-only shared volume.
4. **Exploration** — either you drive the app yourself inside the VM
   window, or Insula runs it unattended in the background and detects
   completion, a hang, or a crash purely from the observed event stream.
5. **Report** — the filtered, target-scoped event trace is turned into a
   plain-language behavioral narrative via the Claude API.

## Requirements

- Apple Silicon Mac, macOS host.
- [Tart](https://github.com/cirruslabs/tart) (`brew install
  cirruslabs/cli/tart`) for VM management.
- Rust (stable, 2024 edition).
- **Recommended terminal: [Kitty](https://sw.kismet.io/kitty/).** Insula's
  background watermark renders at full fidelity, live-resizable, only in
  terminals that speak the Kitty graphics protocol (Kitty itself, Ghostty,
  WezTerm). Any other terminal still works fully — it falls back to a
  re-encoded image approximation — but Kitty is the intended experience.
- An `ANTHROPIC_API_KEY` environment variable, if you want narrated
  reports rather than the raw event log (see *Reports and the Claude API*
  below).

## Getting started

One-time setup — pulls a base macOS image, clones a golden VM, and
provisions it with Insula's sensor and support daemons. This is also the
only place SIP ever gets disabled, and it requires a few manual steps
Apple deliberately can't let you script (Recovery mode, Startup Security
Utility):

```sh
cargo run --bin insula_setup
```

Re-run with `--force` any time the sensor or provisioning scripts change,
to push updates into the already-prepared golden image without redoing
the SIP step.

Then run the actual tool:

```sh
cargo run --bin insula_cli
```

## How to use

Once `insula_setup` has run at least once, every analysis is just:

1. **Run `cargo run --bin insula_cli`.** Press Enter through the welcome
   screen.
2. **Give it a path** — a `.app` bundle, or a bare executable. Insula
   validates that the path exists and is actually runnable on the host OS
   before doing anything else.
3. **Pick how the exploration should happen:**
   - `[1] Manual` — a VM window opens (over VNC) and you use the app
     yourself, however you like.
   - `[2] Unattended` — no VM window. The app is launched unattended
     inside the guest, and Insula watches the sensor's live event stream
     to work out on its own whether the app finished normally, hung, or
     crashed — no API calls involved in that detection, it's read straight
     off observed process/file events.
4. **Insula clones a fresh, disposable VM** from the prepared golden
   image, shares your target app into it read-only, and boots it. This
   clone is single-use — it's destroyed after this run regardless of what
   happens inside it.
5. **In Manual mode**, explore the app however you want inside the VM
   window. When you're done, come back to the terminal and press `Esc` —
   that ends the session and triggers report generation. **In Unattended
   mode**, this happens automatically once the app is detected as
   finished, hung, or crashed — no key press needed.
6. **Insula tears the VM down** and builds the report: it takes the full
   raw sensor log, filters it down to just the target app's own process
   tree (so unrelated system noise never shows up in your report), and
   hands that to the report step below.

### Reports and the Claude API

The filtered event trace can be turned into a plain-language narrative by
the Claude API — set `ANTHROPIC_API_KEY` in your environment before
running `insula_cli` and this happens automatically after every run.

If the key isn't set (or the API call fails for any reason), Insula
doesn't fail the report — it falls back to printing the same filtered
events as a plain chronological timeline instead. Either way you get a
real, complete report; the API only changes whether it's narrated prose
or a raw timeline.

There's no other way to get narrated reports out of a cloned copy of this
repo — report generation is a normal, direct call to the public Claude
API, gated only by that environment variable. It isn't tied to any
particular machine, account, or session; anyone running Insula supplies
their own key.

## Test payloads — read this before touching `insula-test-payloads/`

> **⚠️ `insula-test-payloads/EncryptApp` and `insula-test-payloads/DecryptApp`
> are a real, functional file-encryption / file-decryption pair — a
> working ransomware-style algorithm, included on purpose as a realistic
> sample to test Insula against. This is not a toy or a simulation. If run
> outside the VM, `EncryptApp` will actually encrypt real files on
> whatever machine it's run on.**

**They ship deliberately non-executable** (`chmod a-x`, no execute
permission for anyone) as the default safety state. Before using either
of them *inside the VM* to test Insula, restore the execute bit yourself:

```sh
chmod -R a+x insula-test-payloads/
```

Do this only when you're about to hand one of them to `insula_cli` for
analysis. There's no reason to leave them executable on your host machine
at any other time.

**No warranty, no liability.** These files are provided as-is, for
testing Insula's observation and analysis pipeline in an isolated VM. If
you (or anything else) run `EncryptApp` outside of that isolation and it
encrypts real data, that is entirely on you — we take no responsibility
for any data loss that results from running it with execute permission
granted, in or out of the VM.

If that happens anyway: `DecryptApp` is built as the matching counterpart
and is intended to reverse `EncryptApp`'s encryption. It ships
non-executable for the same reason and needs the same `chmod -R a+x`
before use.

## Status

Actively developed. Consent/launch, VM isolation, live EndpointSecurity
observation, event filtering, and both manual and unattended exploration
modes are built and working. Claude-narrated reports are implemented but
not yet live-verified end-to-end against a real API key.
