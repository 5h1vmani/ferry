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

## 3. Version negotiation

Every connection starts with seven bytes from each side, before anything else:

```text
[5 bytes "FERRY"][u16 highest supported version]
```

Both sides send first, then read, so neither waits on the other. Each side
takes the lower of the two versions. This build supports version 1 only.

This exchange happens in the clear, because the encrypted channel does not
exist yet. That would normally let an attacker force both sides down to an old
version. It cannot happen here, because these exact bytes become the Noise
prologue:

```text
[initiator's 7 bytes][responder's 7 bytes]
```

A prologue is mixed into the Noise handshake hash. An attacker who edits one
byte makes the two sides compute different hashes, so the handshake fails. The
version exchange is unauthenticated when it happens, and authenticated a moment
later.

Implemented in `crates/ferry-core/src/version.rs`.

## 4. Pairing

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
4. Both sides compute the code from the Noise handshake hash, `nonce_a`, and
   `nonce_b`.

The commitment is:

```text
BLAKE3("ferry-pairing-commitment-v1" || static_public_key || nonce)
```

The code is:

```text
BLAKE3("ferry-pairing-code-v1" || handshake_hash || nonce_a || nonce_b)
```

Take the first eight bytes of that hash as a big endian number, reduce it
modulo one million, and show six digits. Eight bytes reduced to six digits
leaves a bias far below one part in a trillion.

Each hash carries its own context string, so no value can be reused as
another.

An attacker must now commit before seeing `nonce_b`, so grinding does not help.
Its chance per attempt is one in the size of the code space.

Implemented in `crates/ferry-core/src/noise.rs`. The test named
`an_initiator_that_changes_identity_is_caught` runs the attack this design
exists to stop.

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

## 5. Connections

Every connection after pairing runs a Noise `KK` handshake, using the stored
static keys.

`KK` proves the initiator's identity in message one, because message one
carries a static-static Diffie-Hellman step that only the real initiator can
compute. The responder's identity is proven when message two decrypts. A device
that is not paired cannot complete the handshake.

The cipher suites are `Noise_XX_25519_ChaChaPoly_BLAKE2s` for pairing and
`Noise_KK_25519_ChaChaPoly_BLAKE2s` afterwards.

Handshake messages travel as `[u16 length][message]`, and a message over 1024
bytes is refused. Real handshake messages are under 200 bytes. The cap stops a
peer from making the other side hold a buffer before it has proved anything.

Once the handshake finishes, transport messages travel as
`[u16 length][ciphertext]`. The Noise specification caps one transport message
at 65535 bytes, and the authentication tag takes 16 of those, so one message
carries at most 65519 bytes of plaintext. The encrypted stream splits longer
writes and joins them again, so the framing layer above never sees that cap.

### Hello

The first frame after a handshake, in both directions, is a hello. Each side
sends its own hello before it reads the peer's, so neither side waits on the
other. The name is shown on the other device and never trusted. Identity is
proven by the handshake's keys, not by this exchange. A peer that sends
anything else first is disconnected.

| Field | Size | Meaning |
|---|---|---|
| `kind` | 1 byte | Always 4. |
| `request_id` | 4 bytes | Always 0. |
| payload | 1 to 64 bytes | One text field: the display name, with no control character. |

## 6. Framing

Designed and implemented. See `crates/ferry-core/src/frame.rs`.

Once a connection is encrypted, every message is a frame:

```text
[u32 payload_len][u8 kind][u32 request_id][payload]
```

| Field | Meaning |
|---|---|
| `payload_len` | Length of the payload only. Checked against the cap before any memory is reserved. |
| `kind` | 1 is a request, 2 is a successful response, 3 is a failed response. |
| `request_id` | Ties a response to its request. Several may be in flight at once. |

The length comes first and is checked before allocation. An unbounded length
would let a peer exhaust memory with four bytes.

Answers may return in any order, because each carries the identifier of the
request it answers. That is what keeps many small files fast.

Framing runs over a plain byte stream. It does not know that the stream is
encrypted, and it does not need to. The Noise layer below splits its bytes into
transport messages.

### Value encoding

All integers are big endian. A byte string or a piece of text carries a `u32`
length first. Text is UTF-8, and invalid text is refused rather than replaced.

A decoder rejects trailing bytes. Extra bytes usually mean the two sides
disagree about the format, so the message is refused rather than half
understood.

The encoding is written by hand rather than taken from a serialisation crate,
because this document has to describe every byte.

Implemented in `crates/ferry-core/src/wire.rs`.

## 7. Limits

Every limit exists so that one peer cannot exhaust the other. Some are reached
during ordinary use, not only under attack. A camera folder holding twenty
thousand photos is normal, and it does not fit in one frame.

### Normative limits

A second implementation must match these two. Everything else is free.

| Limit | Value | Why it is fixed |
|---|---|---|
| Frame payload | 1 MiB plus 64 KiB | A peer cannot send a frame the other refuses to read |
| Plaintext in one Noise message | 65519 bytes | The Noise specification caps a transport message at 65535, and the tag takes 16 |

### Local policy

Everything below is each side's own choice. Two devices need not agree.

If this side caps a read at one mebibyte and the other caps at half that, both
still work. The smaller side refuses, and the caller asks for less. So an
implementation must always be ready to have a request refused for being too
large, and must never assume the peer's numbers match its own.

| Limit | This build's value |
|---|---|
| `read` length in one request | 1 MiB |
| `write` bytes in one request | 1 MiB |
| Entries in one `list` response | 1024 |
| Path length | 1024 bytes |
| Chunks in one manifest | 32768 |

A chunk larger than the `read` limit is fetched with several reads, because
reads carry a byte range.

The values live in `crates/ferry-core/src/limits.rs`, where each is documented
with its reason. They are listed here to show the shape, not to bind anyone.

### Limits that are designed but not built

These are needed and are not yet enforced by any code.

| Limit | Waiting on |
|---|---|
| Requests in flight per connection | Pipelining, which is not built |
| Outstanding response bytes per connection | Pipelining, which is not built |

Two limits that were in this table are now enforced by
`crates/ferry-core/src/tcp.rs`. A socket carries a ten second timeout from
accept until the Noise handshake succeeds. At most eight connections may sit
between accept and a finished handshake, and the ninth is refused before any
byte is read from it.

They are recorded here rather than as constants in `limits.rs`. A constant that
nothing checks reads as protection during a review and provides none.

## 8. File operations

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

The empty path names the shared root itself. It may be listed and stat'ed.
Every other operation on it is refused.

Those checks are lexical. They are not enough on their own. A symlink inside
the root that points outside it passes all of them. So does a FIFO, which would
block the serving thread forever, and so does a device file.

The filesystem layer must therefore also:

1. Resolve every path inside a directory capability that cannot be talked into
   leaving the root. Ferry uses `cap-std` for this rather than hand-written
   `O_NOFOLLOW` handling.
2. Refuse every symlink, even one that points inside the root. A symlink is
   never listed, never opened, and never followed as the last component. This
   is stricter than a symlink check needs to be, and simpler to get right.
3. Check the file type on the open descriptor, not on the path. A check on the
   path can be raced: a regular file is swapped for a FIFO between the check
   and the open, and the open then blocks forever. So a file is opened without
   blocking, its type is read from the handle, and anything that is not a
   regular file is refused before the first read.
4. Skip any directory entry whose name is not valid UTF-8. Such a name cannot
   be sent and then used again, so it is never shown.

Two things the layer does not defend against, stated so nobody assumes it
does. A hard link created from outside the root into it by a local process is
indistinguishable from a real file, and is served. A local process that can
write inside the shared root can change any file at any time, which is what
sharing a folder means.

The lexical rules live in `crates/ferry-core/src/path.rs`. A test there named
`a_validated_path_can_still_escape_through_a_symlink` records the gap that
rule 1 closes. The filesystem rules live in `crates/ferry-core/src/localfs.rs`.

## 9. Transfers

A transfer is a loop of `read` or `write` calls. Pushing is repeated `write`.
Pulling is repeated `read`.

Each transfer has a **manifest**. The manifest lists the file, its size, its
chunk size, the hash of each chunk, and the root hash of the whole file.

### Hashing

Designed and implemented. See `crates/ferry-core/src/chunk.rs`.

Each chunk carries a 32 byte **chaining value**, not a hash. BLAKE3 builds a
binary tree over the input, and the file hash is the root of that tree. A
chaining value is an internal node of that tree. `blake3::hash(chunk)` is a
root hash of the chunk alone, and root hashes do not combine. Chaining values
do.

The values come from the `blake3` crate's `hazmat` interface, using
`set_input_offset` and `finalize_non_root`. They merge back to the file hash
with `merge_subtrees_non_root` and `merge_subtrees_root`.

This is why one pass over a file yields both the per-chunk values and the
whole-file hash. Computing chunk hashes the obvious way would break it, and the
failure would be silent.

The interface constrains the chunk size. A chaining value only means something
for a complete subtree. A subtree must start at an offset that is a multiple of
its own length, and its length must be a power of two multiple of 1024 bytes.

| Setting | Value |
|---|---|
| Chunk size | A power of two |
| Smallest | 1024 bytes, which is the BLAKE3 chunk length |
| Largest | 16 mebibytes, which bounds what a receiver buffers |
| Default | 1 mebibyte |

The `ChunkSize` type enforces those rules, and it is the only way to build a
manifest.

A chaining value also depends on where the chunk sits in the file. A peer
therefore cannot pass off chunk 0 as chunk 2. There is a test for that.

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

## 10. Discovery

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
section 4, which the pairing mode and the commitment step address.

The service type is `_ferry._tcp.local.`. The instance name is sixteen
lowercase hex characters from the system random source. The TXT record holds
one key, `v`, whose value is the protocol version in decimal. Implemented in
`crates/ferry-core/src/discovery.rs`.

Open question. Android needs a `MulticastLock` for multicast receive, and
recent macOS requires a local network permission. Both are confirmed during
phase 1.

## 11. Key storage

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

## 12. Considered and rejected

**QUIC as the network transport.** It offers stream multiplexing and connection
migration, which look like a fit. It requires TLS 1.3, which conflicts with the
Noise choice, and it cannot run over a USB bulk pipe. That would mean two
security stacks for one protocol. On a local network with sub-millisecond
latency, pipelined requests over one stream reach nearly the same result.

**TLS instead of Noise.** See decision record 3.

**A six digit code over plain Noise `XX`.** Forgeable in seconds by an attacker
in the middle. See section 4.

**Content-defined chunking with a rolling hash.** This is what restic and borg
use. It pays off for edited documents and disk images. Ferry moves photos and
videos, which change as whole files or not at all. Fixed chunks plus whole-file
deduplication captures the real win for far less work.

**A push-only protocol.** Simpler, and unable to support a Finder mount.

**Recursive delete in version 1.** The largest single action a peer could
trigger, for a case the user can do folder by folder.
