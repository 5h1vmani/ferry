# Audit 3: the engine contract batches

Date: 11 September 2026.
Scope: the changes behind `docs/engine-contract.md`, built in five batches
on that day. Four audits ran, one per batch of engine work, each on the
committed diff of that batch. Every finding below is fixed in a later
commit with a test, except where the entry says it was kept.

Method: a read-only adversarial review of each diff against the contract,
the protocol document, and the earlier audits. The auditor traced code and
did not run it. The fixes landed in two passes after the batches, so each
fix is a commit of its own on `main`.

## Batch C, ferry-core: named roots, protocol version 2, device kind

No blockers. Nine findings, five fixed in code and four in words.

- A root name could hold a backslash, which no path could then address.
  Refused, with the rule added to the contract.
- Two roots could nest, so a read-only root inside a writable one could be
  written through the other name. Nesting and sharing a folder are now
  refused with `RootsError::RootOverlaps`.
- The hello payload size in the protocol document was wrong. Corrected.
- Case-insensitive root lookup and the read-only `set_mtime` refusal were
  undocumented. Documented.
- A version 1 peer offer being refused had no test. Pinned, with eight more
  tests for behaviours that were correct but unproven.

## Batches B and C, runtime: fields, roots wiring, kind

No blockers. Six findings.

- `usb_port` was set once and never cleared, so USB stayed listed as a
  spare transport after the cable was pulled. Cleared when the forward
  goes away.
- The per-transfer speed counted bytes moved before the attempt, and
  counted local hashing as wire speed. The window now starts at the first
  byte count the attempt sees.
- A Wi-Fi success was recorded only when this device dialled. The
  accepting side records it too. The contract's Wi-Fi rule was rewritten
  to say what the code does: a success within 120 seconds, either way.
- Both shipped defaults put the download folder inside a shared root.
  Kept, and stated in the contract: a person who shares Downloads expects
  the peer to see what landed there.
- `set_download_dir` before `start` was discarded. Kept now, as roots are.
- Chunk counts used this run's chunk size, not the record's. The row keeps
  the record's.

## Batch D: batches and the folder copy

Two blockers, both in how the folder walk trusted the peer.

- A page cursor that never advanced looped for ever, and the older `list`
  call had the same loop with an unbounded entry list. Both now refuse a
  cursor that does not advance and cap pages and entries at 10,000.
- An entry with an empty name walked the folder into itself. The walk
  skips it, and the wire decoder refuses an empty name or one holding a
  slash.

Six more findings: a batched transfer with no batch was shown nowhere on
the Mac; a batch's done count was lost across a restart because finished
records are deleted; a stored count was reserved before it was checked; an
undecodable batch file was never removed; the folder copy button had no
accessible name; and a failed batch showed no reason and no retry. The
last one added `transport`, `error`, and `retry_batch` to item 2.

## Batch E: the access log

No blockers. Five findings and two nits.

- `stop` dropped every pending entry before the serving threads ended.
  They are finished first now.
- Day files were created world-readable. They are `0600` in a `0700`
  folder, like the peer store.
- Every append ran `fsync` under the one lock every connection shares.
  The sync is gone; the store already repairs a torn tail.
- One damaged byte in the middle of a day file deleted every good entry
  after it. A damaged file is set aside and a fresh one started.
- The Mac built a date formatter for every row on every tick. Cached.
- A folder copy logged its bytes twice on the calling side. It logs the
  folder once now, and the contract says so.
- A paged listing walked into a subfolder splits the parent's entry on
  the serving side. Kept, and stated in the store's module doc, because it
  is what keeps the pending table bounded.

## What this audit did not do

Nothing was run on a real phone and Mac pair. `docs/manual-checks.md` task 4
lists what a person should check.
