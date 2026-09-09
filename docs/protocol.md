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

Two devices pair once. Pairing is the only moment when a network attacker can
get in. Everything after it rests on pairing being correct.

### The threat

Noise `XX` alone is not enough. In `XX` the initiator sends its static public
key in message three, after it has seen every other input. An attacker in the
middle runs two handshakes at once. Towards the phone it acts as initiator, so
it can generate static keys until the code shown on the phone matches the code
shown on the Mac.

A six digit code needs about one million X25519 key generations to forge. That
takes seconds on a laptop. This is why Signal's safety number is 60 digits, and
why Bluetooth numeric comparison commits to a nonce before revealing it.

### The fix: commit, then reveal

Each side must be locked in before it sees the other side's input.

1. The initiator picks a random 32 byte `nonce_a`. In the message one payload it
   sends `BLAKE3(static_public_key_a || nonce_a)`. That is a commitment. It
   reveals nothing and it cannot be changed later.
2. The responder picks a random 32 byte `nonce_b` and sends it in the message
   two payload.
3. The initiator reveals `nonce_a` in the message three payload. The responder
   checks the commitment against the static key it now holds. A mismatch aborts
   the handshake.
4. Both sides compute the code as a short value derived from the Noise handshake
   hash, `nonce_a`, and `nonce_b`.

An attacker must now commit before seeing `nonce_b`, so grinding does not help.
Its chance per attempt is one in the size of the code space.

Exact code length and derivation: not designed yet. It will not be shorter than
six digits.

### Pairing rules

- Pairing runs only while the user has opened a pairing mode on the phone. That
  mode times out. Outside it, the phone refuses `XX` handshakes.
- Both screens must show the code, and a person must confirm on both.
- Pairing over the USB cable is allowed and preferred. There is no network
  attacker on a cable, so the grinding attack does not apply.

Each device then stores the other's static public key. This is trust on first
use, with the code protecting the first use.

### Forgetting a device

A device can be unpaired. Unpairing deletes the stored static public key and
every session manifest for that peer. A lost or replaced phone must not stay
trusted forever.

## 4. Connections

Every connection after pairing runs a Noise `KK` handshake, using the stored
static keys.

`KK` proves the initiator's identity in message one, because message one
carries a static-static Diffie-Hellman step that only the real initiator can
compute. The responder's identity is proven when message two decrypts. A device
that is not paired cannot complete the handshake.

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
- The document states how a frame maps onto Noise transport messages. The Noise
  specification caps one transport message at 65,535 bytes, so a large chunk
  spans several. The codec cannot be written without this rule.

## 6. Limits

Every limit below exists so that one peer cannot exhaust the other. Some of
these trigger during ordinary use, not only under attack. A camera folder with
20,000 photos is normal.

| Limit | Why |
|---|---|
| Maximum frame length | An unbounded length exhausts memory |
| Maximum requests in flight per connection | Pipelining without a credit window is a memory attack |
| Maximum outstanding response bytes per connection | Ten thousand reads of one mebibyte each ask for ten gibibytes |
| Maximum `read` length in one request | Bounds the buffer the server must hold |
| `list` returns a page and a cursor | A folder can hold more entries than one frame |
| Handshake timeout | Half-open handshakes must not accumulate |
| Maximum connections before a completed handshake | Any host on the network can otherwise fill the table |

Values: not designed yet.

## 7. File operations

Nine operations. Either side can serve them.

| Operation | Arguments | Returns |
|---|---|---|
| `list` | path, cursor | a page of entries, and the next cursor |
| `stat` | path | one entry |
| `read` | path, offset, length | bytes |
| `write` | path, offset, bytes | bytes written |
| `truncate` | path, length | nothing |
| `rename` | from path, to path | nothing |
| `set_mtime` | path, time | nothing |
| `mkdir` | path | nothing |
| `delete` | path | nothing |

Each entry holds a name, a kind, a size, and a modified time.

`read` and `write` carry a byte range from version 1. Finder must list files
without downloading them, and fetch bytes on demand. A whole-file read cannot
support that.

`rename` exists because Finder saves a file by writing a temporary name and
then moving it. Without `rename`, moving a four gibibyte video would become a
read, a write, and a delete over the network, and it would not be atomic.

`truncate` exists because `write` can extend a file or overwrite part of it,
but it cannot shorten one. Writing a smaller version would leave the old tail
behind.

`set_mtime` exists so that a copied photo keeps its original time.

`delete` is not recursive in version 1. Deleting a directory that is not empty
returns an error. A recursive delete is the largest single action a peer can
trigger, and version 1 does not offer it.

### The shared root

Each side serves exactly one shared root, and never anything above it.

On the Mac the root is a folder the user chooses.

On the phone the root is a fixed list of top-level folders: `DCIM`, `Pictures`,
`Movies`, `Music`, `Download`, and `Documents`. The Android app holds broader
access than that, so limiting the root limits the damage if the app is ever
compromised.

### Paths

A path is relative to the shared root, and uses `/` as the separator.

A receiver rejects any path that is absolute, that holds a `.` or `..`
component, that holds a NUL byte or a backslash, or that is longer than 1024
bytes.

Those checks are lexical. They are not enough on their own. A symlink inside
the root that points outside it passes all of them. So does a FIFO, which would
block the serving thread forever, and so does a device file.

The filesystem layer must therefore also:

1. Open every path with `O_NOFOLLOW_ANY` on macOS, and with `O_NOFOLLOW` on
   each component on Android.
2. Refuse anything that is not a regular file or a directory.
3. Resolve the result and confirm it still sits inside the shared root.

The lexical rules live in `crates/ferry-core/src/path.rs`. A test there named
`a_validated_path_can_still_escape_through_a_symlink` records the gap so that
nobody forgets step 1.

## 8. Transfers

A transfer is a loop of `read` or `write` calls. Pushing is repeated `write`.
Pulling is repeated `read`.

Each transfer has a **manifest**. The manifest lists the file, its size, its
chunk size, the hash of each chunk, and the root hash of the whole file.

### Hashing

BLAKE3, in a way that matters.

The claim that one pass yields both the per-chunk hashes and the whole-file
root hash is true only through the `blake3` crate's `hazmat` interface. The
per-chunk values are subtree chaining values, not `blake3::hash(chunk)`. Values
computed the obvious way never merge into the root hash.

That interface also constrains the chunk size. Each chunk must start at an
offset that is a multiple of the chunk length, and the chunk length must be a
power of two multiple of 1024 bytes.

So the chunk size is a power of two. Its value is not designed yet. Read the
`blake3` crate documentation before writing the chunker.

### Resume

The manifest is a hint. The disk is the truth.

When a connection dies, either side may open a new one and continue the same
session. On resume:

1. The receiver re-hashes the chunks it believes it holds. BLAKE3 runs at over
   one gigabyte per second, so this is cheap.
2. The sender's manifest is authoritative for the file size and the chunk
   hashes.
3. A different root hash or a different size means the source file changed. That
   is a new session, not a resume.
4. Transfer continues from the first chunk that fails verification.

A manifest is never trusted because it is on disk. Another process may have
edited it. A manifest that falsely marks chunks as confirmed would otherwise
produce a corrupt file that looks complete.

### Writing safely

A file being received is written to a temporary name inside the destination
folder. The root hash is verified when the last chunk lands. Only then is the
file renamed to its final name.

Without this, Android's gallery shows half-written videos, and an interrupted
transfer leaves a broken file at the real path.

Ferry never migrates a live stream between transports. See decision record 5.

## 9. Discovery

Devices announce themselves over mDNS on the local network.

Only the phone advertises. The Mac browses. This halves what is broadcast and
keeps the Mac silent on untrusted networks.

Rules for the advertisement:

- The instance name is random, not the device hostname. A default mDNS name
  broadcasts something like "Shiva's Pixel" to everyone in a cafe.
- The TXT record carries the protocol version and nothing else. It must not
  carry the static public key or its fingerprint. A stable key in a public
  record is an identifier that tracks the device across every network it joins.
- The user can turn advertising off.

An mDNS answer is not authenticated. An attacker can answer with its own
address. After pairing, `KK` then fails and the connection stalls, which is
annoying but not dangerous. During pairing it is the attack described in
section 3, which the pairing mode and the commitment step address.

Service type and exact TXT contents: not designed yet.

Open question. Android needs a `MulticastLock` for multicast receive, and
recent macOS requires a local network permission. Both are confirmed during
phase 1.

## 10. Key storage

Each device holds one long-lived X25519 static key pair, plus the public keys
of the devices it has paired with.

The Noise handshake runs inside the Rust core, so it needs the raw private key
in memory. Android Keystore and the Secure Enclave never release raw key bytes,
so the key pair cannot be hardware-backed.

**Android.** Store the key in app-private storage. Exclude it from backup, with
`android:allowBackup="false"` or a `dataExtractionRules` entry. Android Auto
Backup is on by default, so without this the private key is uploaded to the
user's Google Drive. A restore onto a second phone would then create two phones
holding one identity. Wrap the stored key with an AES key held in Android
Keystore.

**macOS.** Store the key as a Keychain item. Keychain access is tied to the
application's code signature. An ad-hoc signature changes on every rebuild, so
a stable signing certificate is needed during development, not only for
release.

## 11. Considered and rejected

**QUIC as the network transport.** It offers stream multiplexing and connection
migration, which look like a fit. It requires TLS 1.3, which conflicts with the
Noise choice, and it cannot run over a USB bulk pipe. That would mean two
security stacks for one protocol. On a local network with sub-millisecond
latency, pipelined requests over one stream reach nearly the same result.

**TLS instead of Noise.** See decision record 3.

**A six digit code over plain Noise `XX`.** Forgeable in seconds by an attacker
in the middle. See section 3.

**Content-defined chunking with a rolling hash.** This is what restic and borg
use. It pays off for edited documents and disk images. Ferry moves photos and
videos, which change as whole files or not at all. Fixed chunks plus whole-file
deduplication captures the real win for far less work.

**A push-only protocol.** Simpler, and unable to support a Finder mount.

**Recursive delete in version 1.** The largest single action a peer could
trigger, for a case the user can do folder by folder.
