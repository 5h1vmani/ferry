# Contributing to Ferry

Ferry is experimental. See the README for its current status before you
build on it. This file covers how to work in the repository.

## The gate script

Run `scripts/gate.sh` before every push. It runs one mode's checks in
order and stops at the first failure.

```
scripts/gate.sh rust     # Format, lint, and test the Rust workspace
scripts/gate.sh gen      # Check that generated files match their source
scripts/gate.sh mac      # Build the Mac app
scripts/gate.sh android  # Build the Android app's Kotlin sources
scripts/gate.sh all      # Run all four, in that order
```

Each step prints `gate: <step> ok` or `gate: <step> FAIL (exit N)`. Full
output goes to `target/gate.log`. On failure, the last 40 lines of the
failing step print to your terminal.

## Test rules

- **One test file per item.** A new test goes in a file named for the
  thing it tests, never appended to an existing shared file. A shared
  file causes merge conflicts and grows past the point anyone can read
  it in one sitting.
- **No test asserts on the machine's speed.** Do not write a test that
  waits out a timer and checks it fired near the deadline; a slow CI
  runner makes that test flaky. Instead, give the code under test a way
  to set the window directly (a test-only knob), or wait on a real
  event the code already exposes, and give any real wait a budget of at
  least three times its own time on a fast machine.

## Commit subjects

A commit subject says what changed, in plain words, so a reader
understands it without opening the diff. Start with an imperative verb:
"Add", "Fix", "Drop", "Widen", "Replace". Name the file or the thing
that changed. Keep it under about 70 characters, with no trailing
period. For example: `Fix a signing error on a fresh clone` or `Drop a
settle sleep the resume test does not need`.

## Running the resume sweep

`crates/ferry-runtime/tests/resume_sweep.rs` cuts a pull's connection at
every byte position and checks that resume refetches at most one chunk.
It is marked `#[ignore]` because it is slow, so it does not run in
`scripts/gate.sh rust` or in CI's normal push. Run it by hand:

```bash
cargo test -p ferry-runtime --test resume_sweep -- --ignored
```

## Signing the Mac app

`macos/Signing.xcconfig` defaults to ad hoc signing, so `scripts/gate.sh
mac` builds with no Apple developer team. If you have a team and want a
stable signature across rebuilds (so the Keychain and the local network
prompt stop asking every time), create `macos/Local.xcconfig`, which is
gitignored:

```
DEVELOPMENT_TEAM = <your team ID>
CODE_SIGN_IDENTITY = Apple Development
CODE_SIGNING_ALLOWED = YES
```

## Where the process history lives

`docs/agent-runs.md` is the record of the batch process that built most
of this repository with AI agents. Its numbered rules are cited by file
and line across the Rust source and the audits, so the numbers stay
fixed; the rules above are the ones that also apply to a human
contributor.
