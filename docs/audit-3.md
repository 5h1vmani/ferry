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

# The second run, same day

Five more batches landed on 11 September 2026: the runtime limits (item
16), automatic copying (14), push (5), the mount in two halves (6), and QR
pairing on the engine and Mac side (12). Six audits ran, one per batch of
engine work. Every finding below is fixed in a later commit with a test,
except the three named at the end.

## The runtime limits

One blocker: a manifest request read a whole file with no bound before it
started, and a file over 32 GiB overflowed the manifest after the read.
The length is read first, the chunk size follows it, and over 512 GiB is
refused before any read. Also fixed: a self-inconsistent manifest retried
forever; dials had no connect timeout; the peer store's in-memory add had
no cap. A worry about SIGPIPE was checked against the standard library and
needed no code.

## The read-only bridge

Three blockers, all the same class: a local process could exhaust memory
or threads before the password check, through an unbounded body, an
unbounded header, or unbounded connections. Every request is now bounded
before authentication and every socket has a timeout. Also fixed: a bad
range answered 200 with a wrong length; a mid-body failure desynced the
connection; bridge requests wrote no entry in the Mac's log; the sidecar
store, lock table and cache had no bounds; the device name went into a
filesystem path; the NetFS call lacked the no-UI and loopback options.

## Automatic copying

One blocker: every completed pull rewrote and synced the whole held
index, so a phone with thousands of new files drove tens of gigabytes of
disk writes. Rows are appended now. Also fixed: the run slot was freed
before the batch ended, so a reconnect could copy files twice; lookups
scanned every row; the stores were world-readable; a phone file replaced
a file the person had put at the same name; the Running state was never
shown.

## Push

One blocker: a peer that acknowledged writes and stored nothing made the
sender resend forever, and an edited local file hit the same loop. A full
rewrite that still fails is fatal now, and the manifest is rebuilt when
the local file changes. Also fixed: the log entry was written before any
byte moved; the peer's byte count was trusted; two pushes to one path
shared one partial file.

## QR pairing

No blockers. Fixed: a stranger's wrong nonce ended a live offer; a scanned
payload could make the phone dial any number of addresses; the name the
person confirmed was not the name stored; the offer listed every
interface; the nonce compare was not constant time; the offer version was
never checked. The protocol document now states that the scan method's
protection is the two minute window and the single-use nonce, and that
the name shown is a label the phone chose.

## The mount's write half

Three blockers: the folder delete walk counted files but not folders, and
the folder copy walk shared the flaw; a save stamped the new modified time
before it verified; the spool folder had no total bound and leaked files
on a dropped connection. Also fixed: a second lock replaced the first;
locks did not cover ancestors or a move's destination; a bad date in a
property update answered 200; a partial or a sidecar could be moved onto a
real name; copy ignored the overwrite header.

## Still open after this run

- A dial that is already blocked in `connect` is not interrupted by
  `stop` for the scan method; `stop` waits for the connect timeout.
- Push's own manifest build does not update the row's chunk size, the
  same fault fixed for pulls.
- `push_files` has no in-flight guard for two batches to one path, which
  `push` now has.

Nothing in this run has been used on a real phone and Mac pair.
`docs/manual-checks.md` task 4 lists what a person checks.
