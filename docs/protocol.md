# The Ferry wire protocol

Status: draft. Version 1 is not frozen. Sections marked "not designed yet" hold
no commitment.

This document describes what two Ferry devices say to each other. It is written
so that someone could build a second implementation from it.

## 1. Model

Ferry is a small remote filesystem plus a transfer engine that sits on top.

Both devices are peers. Either side can serve file operations, and either side
can call them. There is no fixed client and no fixed server.

A **connection** is one encrypted byte stream between two paired devices. A
**session** is one file transfer. A session can outlive many connections.

## 2. Transports

The protocol needs only a reliable, ordered byte stream in both directions. It
does not care which transport provides that stream.

| Transport | Stream |
|---|---|
| Local network | A TCP connection, found over mDNS |
| USB, developer route | A TCP connection through an adb tunnel |
| USB, Android Open Accessory | The bulk in and bulk out endpoints |

## 3. Pairing

Two devices pair once.

The devices run a Noise `XX` handshake. Each side then shows the user a short
code derived from both static public keys. The user confirms that both screens
show the same code. Each device stores the other's static public key.

This is trust on first use, with the code protecting against an attacker in the
middle. Syncthing and Signal use the same shape.

Exact code derivation: not designed yet.

## 4. Connections

Every connection after pairing runs a Noise `KK` handshake, using the stored
static keys.

`KK` proves both identities from the first message. A device that is not paired
cannot complete the handshake.

Noise cipher suite: not designed yet.

## 5. Framing

Not designed yet.

Requirements the framing must meet:

- The first bytes carry a protocol version, so two versions can negotiate.
- A frame carries a request identifier, so several requests can be in flight at
  once. Pipelining is what keeps many small files fast.
- A frame declares its length before its body, and the receiver enforces a
  maximum. An unbounded length lets a peer exhaust memory.
- Errors are structured values, not text. A caller must be able to act on the
  error without parsing English.

## 6. File operations

Six operations. Either side can serve them.

| Operation | Arguments | Returns |
|---|---|---|
| `list` | path | entries, each with name, kind, size, modified time |
| `stat` | path | one entry |
| `read` | path, offset, length | bytes |
| `write` | path, offset, bytes | bytes written |
| `mkdir` | path | nothing |
| `delete` | path | nothing |

`read` and `write` carry a byte range from version 1. Finder must list files
without downloading them, and fetch bytes on demand. A whole-file read cannot
support that.

### Paths

A path is relative to the shared root, and uses `/` as the separator.

A receiver rejects any path that is absolute, that holds a `.` or `..`
component, that holds a NUL byte or a backslash, or that is longer than 1024
bytes. Without those checks a peer could ask for a file outside the root.

The rules live in `crates/ferry-core/src/path.rs`, with tests.

## 7. Transfers

A transfer is a loop of `read` or `write` calls. Pushing is repeated `write`.
Pulling is repeated `read`.

Each transfer has a **manifest**. The manifest lists the file, its size, its
chunk size, the BLAKE3 hash of each chunk, and the BLAKE3 root hash of the
whole file. Both sides persist the manifest and record which chunks are
confirmed.

BLAKE3 is used because one pass over the file yields both the per-chunk hashes
and the whole-file root hash. Its tree structure makes that free.

Chunk size: not designed yet.

### Resume

When a connection dies, either side may open a new one and continue the same
session. The two sides exchange manifests and continue from the first
unconfirmed chunk.

Ferry never migrates a live stream between transports. See decision record 5.

## 8. Discovery

Devices announce themselves over mDNS on the local network.

Service type, and the contents of the TXT record: not designed yet.

Open question. Android needs a `MulticastLock` for multicast receive, and
recent macOS requires a local network permission. Both are confirmed during
phase 1.

## 9. Considered and rejected

**QUIC as the network transport.** It offers stream multiplexing and connection
migration, which look like a fit. It requires TLS 1.3, which conflicts with the
Noise choice, and it cannot run over a USB bulk pipe. That would mean two
security stacks for one protocol. On a local network with sub-millisecond
latency, pipelined requests over one stream reach nearly the same result.

**TLS instead of Noise.** See decision record 3.

**Content-defined chunking with a rolling hash.** This is what restic and borg
use. It pays off for edited documents and disk images. Ferry moves photos and
videos, which change as whole files or not at all. Fixed chunks plus whole-file
deduplication captures the real win for far less work.

**A push-only protocol.** Simpler, and unable to support a Finder mount.
