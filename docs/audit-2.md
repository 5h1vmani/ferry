# Audit 2: the runtime engine

Date: 10 September 2026.
Scope: the whole of `crates/ferry-runtime`. It is the engine both apps link,
and it was written that day. The core it sits on was audited in
`docs/audit-1.md`.

Method: an adversarial review with a stated threat model, asked for concrete
failures only. The auditor reproduced thirteen of the fourteen findings by
running code before reporting them. One was read from the code and is marked.
Every fix carries a regression test that failed before the fix and passes
after, except two that are marked. The fixes landed as one commit, `5c54de7`,
because they share the same files and the same threads. The core change for
finding 11 is a separate commit, named in that row.

## Findings

| # | Where | What could happen | How it was shown | Fix |
|---|---|---|---|---|
| 1 | guard | `stop` spun one core at full speed and never returned. The stop-aware stream reported `Interrupted`, which `read_exact` and `write_all` retry at once. | Reproduced. `stop` hung until the test was killed at 45 s. | The stream reports `ConnectionAborted`, which is never retried. |
| 2 | engine | `stop` waited up to five minutes when one side had confirmed a pairing and the other had not. The name exchange sat in a socket read that nothing could wake. | Reproduced. Hung until killed. | The name exchange runs on a helper thread with a ten second deadline. `stop` ends the wait at once. The helper thread is left behind like a serving thread and ends when its stream fails. |
| 3 | engine | `forget` could not reach a peer that finished the handshake and then held its `hello` back. The off switch was registered after the names were exchanged. | Reproduced. A read after `forget` answered `NotFound`, so files were still served. | The switch is registered as soon as the handshake proves who is calling. The serving path also asks the paired list once more after the switch is in place, so a `forget` that lands in the short gap before registration closes the connection. That second check was added in review, not by the auditor, and is covered by the same test. |
| 4 | engine | `stop` left the serving switches on and the shared root open, so an open connection kept serving files after the app had stopped. | Reproduced. A read after `stop` still answered. | `stop` flips every switch and takes the shared root away before it joins anything. |
| 5 | transfer | An interruption in the first pass left no record and an orphan `.part` file. The next run started from the first byte or found nothing. | Reproduced with finding 1 removed. Failed in 1.5 s. | A first pass record holds the source, its size and modified time, the chunk size, and the count of verified chunks. A restart re-hashes the partial file up to that count and carries on. |
| 6 | engine | Two engines on one data folder each held the whole peer list and each wrote it whole. The second to write put back a device the first had forgotten. A failed record delete had the same effect. | Reproduced. | A lock file made with `create_new` refuses the second engine and names the file to clear after a crash. A record whose device is no longer paired is dropped on its first attempt. |
| 7 | engine | The listener was called after `stop` returned, and a listener that held the engine made a ring of references, so `Drop` never ran. | Reproduced. | A new `notify` module holds the listener in a slot that `stop` empties after it joins every thread. The crate docs say the app must call `stop`. See the one case below that stays open. |
| 8 | engine | The candidate list had no cap, and each new candidate sent the whole list to the app, so the work grew as the square of the count. | Reproduced. | Capped at 32. The list is reported at most once a second. |
| 9 | engine | A second `pick_candidate` while a dial was running started a second handshake. Two codes showed, then `Failed` and `Confirmed` in turn. A confirm after the watchdog stored the device anyway. | Reproduced. The second pick returned `Ok`. | A dial in progress refuses a second pick and refuses an inbound pairing with `Runtime::PairingBusy`. A confirm that finds pairing already over stores nothing. |
| 10 | transfer | Every transfer got its own thread with no cap, and every chunk called the app. | Reproduced on the callback count. | Four workers share a queue. A transfer beyond the fourth is reported as `Queued`. `devices_changed` and `transfers_changed` arrive at most once every 250 ms, and the last change always arrives. |
| 11 | transfer, core | A paired peer that answered one byte per read held a worker until the connection died. | Reproduced with the cap removed. Failed in 9.1 s. | The first pass gives up on a chunk after 64 reads or 30 s. The resume read loop in `ferry-core` had the same shape and now stops after the same 64 reads, from one constant, and returns a short result that the caller already treats as `ShortRead`. A unit test covers the core loop. No engine test walks it, because the engine never reaches that loop with chunks outstanding today: a ready record is written only after every byte has landed. `e046708` |
| 12 | transfer | A record was written with `std::fs::write`, which truncates first. A crash in the middle left a short file that never decoded again. | Reproduced by a structural test. | A new `record` module writes through a random temporary name, `fsync`, then rename. A test proves no other file writes a record. |
| 13 | engine | The state lock was held across the peer list write, which calls `fsync`. Any peer that sent a new name stalled every thread for as long as the disk took. | Read from the code. | The list is cloned, changed, and written under its own writer lock. The state lock is taken only to swap the result in. No regression test, because the cost is timing and not an outcome. |
| 14 | engine | A device with no stored peer dropped every connection before the handshake. A device with one ran the handshake. A stranger could tell the two apart. | Reproduced. A device with no peer sent zero bytes. | A device with no peer runs the handshake against a fresh key that nobody holds, so both cases look the same from outside. |

## Angles found safe

- A listener that calls back into the engine from inside a callback does not
  deadlock. Every callback runs with no lock held.
- A forgotten device cannot reconnect. Its key is gone from the list the
  responder chooses from.
- A wrong guess by the responder about which peer is calling leaks nothing.
  The handshake fails the same way for every wrong key.
- `adb` forwards are removed on `stop`, and a cable already unplugged does
  not make that an error.
- The discovered address list is capped at sixteen.
- A record that does not decode is skipped. It cannot crash the engine.

## Named and not fixed

- One callback may still arrive in the instant after `stop` returns. A
  serving thread is not joined, so one that is ending at that moment can
  make one last `devices_changed` call. Holding the listener lock across the
  call would fix it, but a listener that calls back into the engine would
  then deadlock on that lock. The crate docs name the case.
- A paired peer that is hostile during a resume is bounded, not fast. Each
  read has the five minute idle timeout from `tcp.rs`, and a chunk allows 64
  reads, so the worst case is about five hours to fail one chunk. The first
  pass has a 30 s deadline per chunk as well. A peer that was trusted with
  the files and is later compromised is the threat here, and the engine
  retries with backoff after the failure.
- With two or more stored peers, the responder guesses which one is calling
  by address and falls back to the first. A reconnect from a new address by
  any peer but the first is refused until it pairs again. This is the phase
  2 item in `PLAN.md`.

## What the tests now walk

`tests/engine_paths.rs` holds one test per finding, plus two that already
passed and are kept as coverage: a transfer that pauses when the link breaks
and finishes when it returns, and a device that is not reachable refusing a
new connection. `tests/two_engines.rs` still holds the three end to end tests
from before. The whole workspace gate ran once after both commits, with and
without the testing feature, and passed.
