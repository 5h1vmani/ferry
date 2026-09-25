# Documentation index

What each file in `docs/` covers, so a reader can tell reference material
from historical record.

## Reference

- [`protocol.md`](protocol.md): the wire protocol. Frames, the handshake,
  pairing, chunking, and version negotiation, written so someone could
  build a second implementation.
- [`decisions/`](decisions): one short, dated record per hard design
  choice, eleven so far. The README links every one.
- [`engine-contract.md`](engine-contract.md): the field-by-field contract
  between the Rust engine and the two apps' designed screens.
- [`design.md`](design.md), [`components.md`](components.md),
  [`ia.md`](ia.md), [`jobs.md`](jobs.md), [`voice.md`](voice.md): the
  design system both apps follow. Layout and colour, components, screen
  states, the jobs a person does with Ferry, and the rules for its words.
- [`toolchain.md`](toolchain.md): how to install what a fresh machine
  needs to build Ferry.
- [`spike-0-findings.md`](spike-0-findings.md): what the project's first
  spike answered before the real build started. Cited by decision
  record 8.

## Process history

These record what happened and why, rather than the current design.

- [`manual-checks.md`](manual-checks.md): the device checklist. What a
  person checks by hand, and what each check found.
- [`ux-fix-plan.md`](ux-fix-plan.md): the 16 September 2026 UX fix plan
  and its status.
- [`agent-runs.md`](agent-runs.md): the internal record of the batch
  process that built most of this repository with AI agents.
  [`../CONTRIBUTING.md`](../CONTRIBUTING.md) states the parts of it that
  also apply to a human contributor.

## Audits

Findings from every review run against the code. "Fable" and "Sonnet"
name the AI models that ran a review; the rest are dated read-only
passes, some by a person and some by an agent.

- [`audits/audit-1.md`](audits/audit-1.md): the transport and storage
  modules, `localfs.rs`, `tcp.rs`, `peers.rs`, `adb.rs`, and
  `discovery.rs`.
- [`audits/audit-2.md`](audits/audit-2.md): the whole of
  `crates/ferry-runtime`, the engine both apps link.
- [`audits/audit-3.md`](audits/audit-3.md): the five batches behind
  `docs/engine-contract.md`.
- [`audits/fable-engineering.md`](audits/fable-engineering.md): the Rust
  engine, both apps, the scripts, and CI, read for engineering issues such
  as thread spawns, bounds, and duplicated constants.
- [`audits/fable-lifecycle.md`](audits/fable-lifecycle.md): the engine's
  lifecycle and locks, and both apps' wrappers around it.
- [`audits/fable-security.md`](audits/fable-security.md): pairing, the
  Noise wire, the loopback WebDAV bridge, the phone's documents provider,
  and network trust.
- [`audits/fable-ux.md`](audits/fable-ux.md): both apps as a person meets
  them, from first run through every error row in `design/errors.json`.
- [`audits/kotlin-lifecycle.md`](audits/kotlin-lifecycle.md): the phone
  app's engine object, its lifecycle, and its scan-to-pair path.
- [`audits/principles.md`](audits/principles.md): single source of truth,
  duplicated code, dead code, silent failure, stale comments, and
  threading, across the whole repository.
- [`audits/principles-fixes.md`](audits/principles-fixes.md): the fix
  pass for the findings in `principles.md`.
- [`audits/third-run-engine.md`](audits/third-run-engine.md): the engine
  side of the third 11 September build, items 17 to 19.
- [`audits/ux-gestures.md`](audits/ux-gestures.md): the phone's share
  target and cache handling, and the Mac's drop targets and Finder
  service, from the 16 September UX fix plan.
- [`audits/oss-readiness.md`](audits/oss-readiness.md): licences, the
  project name, public docs, git history, and GitHub settings, reviewed
  ahead of making the repository public.
- [`audits/oss-looks.md`](audits/oss-looks.md): both apps and the
  repository as a first-time visitor meets them, including contrast and
  copy.
- [`audits/oss-capability.md`](audits/oss-capability.md): every factual
  claim in the README checked against the code, and the attack surface a
  public release exposes.
