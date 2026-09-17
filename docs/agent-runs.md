# How the agent runs work

Ferry is built in batches by agents, with one orchestrating session that
writes the contract, launches builders, merges, and audits. This file holds
the rules that came out of the two runs on 11 September 2026. Each rule
cost real tokens, a bad push, or a false test result before it was written
down. A run that ignores one will pay for it again.

## Roles

- The orchestrator decides. It writes `docs/engine-contract.md` items,
  chooses batches, resolves merges, and reads gate output. It reads diffs
  only when a gate fails twice.
- Builders implement. They get the contract item, the files to read, the
  tests to add, and the gate to run. They commit on a branch and never push.
- Auditors read. They get a commit range and a threat model, run nothing,
  and report findings with a file, a line, a cause, and the smallest fix.
- Fix passes implement audit findings, one commit per finding, with a test
  that fails before the fix.

## Rules

1. **The gate is a script.** Every builder and the orchestrator run the same
   gate from one script that exits non-zero on any failure. That script is
   `scripts/gate.sh`, with modes `rust`, `rust-quick`, `gen`, `mac`,
   `android`, and `all`. A gate written in prose costs tokens and let a
   broken tree through twice, because a grep over test output has grep's
   exit code, not cargo's.
2. **Push only on a real exit code.** Run the gate into a log, save its
   exit code, and push only when it is zero. After any merge of two
   branches, run the affected crate's tests before pushing, even when each
   branch was green on its own. Merges broke the tree twice in one day.
3. **One test file per item.** New tests go in a file named for the item,
   never appended to a shared file. Three merges conflicted only because
   every batch appended to the same test file. A test goes in the file of
   its concern, and a file over a thousand lines is split before the next
   batch.
4. **One cargo target directory per worktree.** Two worktrees sharing a
   target directory can link against each other's stale crate. The build
   looks successful and is wrong. Accept the one-time dependency rebuild.
5. **Auditors write to the repo.** Findings go to `docs/audits/<batch>.md`,
   so the fix pass reads the file and the audit record exists for free.
6. **A constant or rule invented in a fix goes into the contract in the same
   commit.** Numbers that live only in a prompt drift.
7. **Audit before merge when the batch touches the wire or security.** Main
   held known blockers for the length of a fix pass otherwise.
8. **Ask for the cheap test.** A pure-function test proves what a 32 GiB
   sparse-file test proves, in milliseconds instead of minutes. No test
   sends packets off the machine.
9. **A comment is not a fix.** Ask for a test that fails without the change.
10. **Report only what changes a decision.** Seconds per test file were
    reported for every batch and used once. Report tests over ten seconds
    and gate failures verbatim, nothing else.
11. **Every agent starts by fast-forwarding to main.** The worktree tool can
    create a worktree from an old commit.
12. **A commit names who made it.** The trailer on a commit names the model
    that wrote the diff, not the session that launched it. The orchestrator
    is accountable through the merge and the push, which it performs.
13. **Main is pushed before a builder is launched.** Builders fast-forward
    from origin, not from local main. Two builders today found origin
    behind local main and had to decide for themselves whether to wait or
    carry on.
14. **A test's time budget is at least three times its time alone.** A
    test that took nine to ten seconds alone, with a ten second budget,
    failed twice under the load of parallel builds today.
15. **An app gate runs the packaging step, not only the compile.** The
    Android compile task never merged the manifest, so a bad provider or
    service element would have passed the gate and failed only at
    install.
16. **No test asserts on the machine's speed.** A wait's budget is at
    least three times its own time on a fast machine. A window, such as a
    cache lifetime or a deadline, is removed with a hidden test knob,
    never waited out with a sleep timed to land just past it. A count or
    a state read that races background work, such as a prefetch thread or
    a shared connection pool, waits on an observable the engine already
    exposes before it reads. Three tests failed on CI's slow x86 runner in
    one week from this same class, each fixed and pushed alone before the
    next one showed. The proof that the whole class is fixed is one run
    of the affected crate's test suite under `--cpus=1` in a Linux
    container, not a single test passing on a fast machine.

## What a builder prompt holds

The contract item to read, the files to read first, the facts the
orchestrator already verified with their locations, the rules above that
apply, the tests to add, the gate, which files other builders are editing
at the same time and where to put additions so the merge stays clean, the
commit subjects, and a report format under a stated line count. Nothing
else.
