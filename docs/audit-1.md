# Audit 1: the transport and storage modules

Date: 10 September 2026.
Scope: `localfs.rs`, `tcp.rs`, `peers.rs`, `adb.rs`, `discovery.rs`, and
how they compose. These five were written that day and formed the whole new
attack surface. The rest of the crate was audited earlier.

Method: an adversarial review with a stated threat model, asked for concrete
failures only. Every finding below was reproduced by the auditor before it was
reported, except one that is marked as read from the code. Every fix carries a
regression test that failed before the fix and passes after.

## Findings

| # | Module | What an attacker could do | How it was shown | Fix |
|---|---|---|---|---|
| 1 | localfs | Swap a regular file for a FIFO between the path check and the open. The serving thread then blocks forever. | Reproduced. 1017 reads, then a hang. | The type is read from the open handle, never from the path. Opened with `O_NONBLOCK`, checked, then cleared. `73816c1` |
| 2 | tcp | Send one byte per timeout window and hold a handshake slot for hours with no key. Also, one silent client made every peer behind it wait the full timeout. | Reproduced. A 300 ms timeout let accept run 1.5 s; a silent client delayed the next peer by the whole timeout. | One deadline bounds the whole handshake however many reads it takes. accept does no I/O. `8eb6089` |
| 3 | peers | Open the key file in the moment it is created with mode 0644, or plant a symlink at its predictable temporary name, or read the key from freed memory. | All three reproduced. A watcher thread saw 0644 on every run. | Created with mode 0600 and `create_new` under a random name, written and synced through one handle. Buffers sized up front and wiped. `728ec81` |
| 4 | localfs | Send a 20 byte list request and receive a full directory scan and sort in return, every page. | Reproduced. 118 ms a page at sixty thousand entries. | Sorted once at cursor zero, served from a small cache for the rest of the run. `73816c1` |
| 5 | adb | A chatty adb fills a pipe and never exits, so a correct call fails after ten seconds. An empty PATH element yields a relative binary that resolves against the working directory. | Both reproduced. | Both pipes drained on threads from spawn, capped at one mebibyte. Only absolute binaries accepted or run. `63debc4` |
| 6 | tcp | A paired peer that is later compromised finishes the handshake and sends nothing, holding a thread forever, because timeouts were cleared after the handshake. | Read from the code. | A five minute idle read timeout after the handshake. `8eb6089` |

## Angles found safe

- Symlink containment holds. cap-std refused a read through a symlink to
  `/etc` and replaced a planted symlink on rename rather than following it.
- A frame length lie costs nothing. The cap is checked before allocation.
- The advertisement leaks no hostname. The host name is built from the random
  instance.
- A hostile mDNS answer only stalls a connection. The Found event carries no
  key, so the attacker still cannot pass the handshake.
- The pending counter cannot drift. The only decrement is a Drop.

## Named and not fixed

- A hard link from outside the shared root into it is served. cap-std cannot
  tell a hard link from a file. It needs a local process to create, and it is
  recorded in `docs/protocol.md` as something the layer does not defend
  against.
- Per-peer connection caps belong to a server loop that does not exist yet.

## What the audit found about the tests

The end to end test served only the in-memory filesystem, so it proved nothing
about the real one, which is where findings 1 and 4 lived. A second end to end
test now moves a file between two real directories.
