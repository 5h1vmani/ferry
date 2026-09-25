# The Ferry wire protocol

Status: draft. Version 2 is not frozen. Sections marked "not designed yet" hold
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

Every connection starts with eight bytes from each side, before anything else:

```text
[5 bytes "FERRY"][u16 highest supported version][u8 mode]
```

Both sides send first, then read, so neither waits on the other. Each side
takes the lower of the two versions. This build supports version 3 only. The
oldest and the newest version it speaks moved together, from 2 to 3, in one
step: version 3 changes the wire, so no build in the field should read version
3 bytes as version 2, and a stale build must fail with `NoSharedVersion`
rather than misread the extra byte.

The mode byte names which Noise pattern the initiator is about to run: `0`
for `KK`, an ordinary connect between paired devices; `1` for `XX`, pairing by
a code shown on both screens; `2` for `IK`, pairing by a code scanned from the
other screen. Only the initiator's byte means anything, since it is the side
that picks the pattern; the responder sends `0` as a filler, to keep the
exchange a fixed length in both directions. A responder that reads the
initiator's byte builds a Noise handshake state for that exact pattern before
it reads a single byte of it, since a handshake state is built for one
pattern and cannot be redirected once it exists.

This exchange happens in the clear, because the encrypted channel does not
exist yet. That would normally let an attacker force both sides down to an old
version, or claim one pattern and run another. It cannot happen here, because
these exact bytes become the Noise prologue:

```text
[initiator's 8 bytes][responder's 8 bytes]
```

A prologue is mixed into the Noise handshake hash. An attacker who edits one
byte, of the version or of the mode, makes the two sides compute different
hashes, so the handshake fails. The version and mode exchange is
unauthenticated when it happens, and authenticated a moment later.

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

### Pairing by scanning a code

A second way to pair: the Mac draws a code, and the phone's camera scans it.
Scanning proves possession earlier than the six digit code does, and needs no
comparison by a person, because the phone learns the Mac's static key from the
screen itself, not from the network.

This runs Noise `IK`. The initiator already knows the responder's static key,
so it sends its own static key in message one, encrypted, instead of waiting
for message three the way `XX` does:

```text
-> e, es, s, ss
<- e, ee, se
```

An attacker in the middle cannot complete this handshake without the Mac's
real private key, because the `ss` term only produces the right result when
the initiator's static key is genuine. There is no code to grind, so decision
record 6's commit and reveal step is not needed here: the pre-shared key is
what closes the gap `XX` alone leaves open.

Message one's payload carries two things: a 16 byte nonce, and the
initiator's hello, encoded exactly as section 5's hello frame encodes a name
and a kind. The nonce proves this is the offer just scanned, not an older,
photographed one: it is made fresh for each offer, dies with it, and is
checked against the one live offer before the handshake is allowed to
finish. Carrying the hello in message one, rather than after the handshake,
lets the Mac show who is asking before anyone confirms anything, with no
second hello needed for that direction.

The offer the Mac draws as a QR code is ASCII text:

```text
"FERRY1:" then base64url of:
  version(1)
  static key(32)
  expiry(8)           -- Unix seconds, big endian, signed
  nonce(16)
  address count(1)
  for each address: tag(1, 4 or 16) then that many bytes of IP then port(2)
```

Only the addresses the phone can reach over Wi-Fi are listed; over the cable
the phone cannot reach the Mac by IP at all. An offer is good for two
minutes and for one scan: the Mac accepts an `IK` handshake only while it is
still showing the offer, and only for the one nonce in it, so a stale or
reused code is refused once message one is decrypted, not before.

Both sides ask a person to confirm, the same as the code method. The Mac
shows the phone's name from the hello, and a person accepts or refuses.
The phone shows the Mac's name from the hello and asks the same question.
Each side stores the other only after its own confirm. A phone that scanned
a stranger's code therefore sees the stranger's name before it trusts
anything. This changed on 11 September 2026; before, the scan was the
phone's only answer.

The scan method's protection is the two minute window and the single-use
nonce, not a value a person compares: the name shown on the Mac is only a
label the phone chose for itself. Someone who wants to compare a value both
sides independently made should use the code method instead.

Implemented in `crates/ferry-core/src/offer.rs` for the payload, and
`crates/ferry-core/src/noise.rs` for the handshake.

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

The cipher suites are `Noise_XX_25519_ChaChaPoly_BLAKE2s` for pairing by
code, `Noise_IK_25519_ChaChaPoly_BLAKE2s` for pairing by scanning a code,
and `Noise_KK_25519_ChaChaPoly_BLAKE2s` afterwards.

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

Since version 2, the payload also carries the sending device's kind: a
phone or a Mac. It is shown alongside the name and is likewise never
trusted for anything. Do not confuse this with the frame's own `kind`
field below, which names the frame type and is unrelated.

| Field | Size | Meaning |
|---|---|---|
| `kind` | 1 byte | Always 4. |
| `request_id` | 4 bytes | Always 0. |
| payload | 6 to 69 bytes | The display name as a length-prefixed piece of text (a `u32` length, then 1 to 64 bytes, no control character), then one byte for the device's kind: 1 is a phone, 2 is a Mac. |

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
| `read` length in one request | 1 MiB (`MAX_READ_LEN`) |
| `write` bytes in one request | 1 MiB (`MAX_WRITE_LEN`) |
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

`delete` is not recursive in version 2. Deleting a directory that is not empty
returns an error. A recursive delete is the largest single action a peer can
trigger, and version 2 does not offer it.

### Named roots

Since version 2, each side serves one or more named roots, not one shared
folder. A peer never sees anything above any root, and never sees a root's
real path, only its name.

Every path begins with a root's name as its first segment: `Desktop/Q3
notes.md` names a file inside the root called `Desktop`. The first segment
finds its root ignoring case. `list("")` returns one directory entry per
root: the root's name, size 0, and the modified time of its folder, in one
page, with no cursor. `stat("")` is a directory entry for that same top
level.

A path whose first segment names no root is `NotFound`. Writing, truncating,
making a directory, deleting, or renaming anything inside a root marked not
writable is `PermissionDenied`. Setting a modified time inside a root marked
not writable is `PermissionDenied` too. Creating, deleting, or renaming a
root itself, addressed by a path of exactly one segment, is
`PermissionDenied` as well: a root is configured on the device, not made or
removed through file operations. `rename` across two different roots is
`Unsupported`; a file moves within a root, never between two.

On the Mac each root is a folder the user chooses. On the phone each root is
a folder such as its internal storage. No two roots share a folder or nest
one inside the other.

Implemented in `crates/ferry-core/src/roots.rs`, which dispatches each call
to a `LocalFs` per root; `LocalFs` itself is unchanged and still serves one
folder.

### Paths

A path is relative to the root it names, and uses `/` as the separator.

A receiver rejects any path that is absolute, that holds a `.` or `..`
component, that holds a NUL byte or a backslash, or that is longer than 1024
bytes.

The empty path names the top level: every root, together. It may be listed
and stat'ed. Every other operation on it is refused.

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

**Network trust.** A device trusts a Wi-Fi network by its name alone. A
network with the same name as a trusted one is trusted too. Nothing else
about the network is checked. A stranger could set up a copied network
on purpose. Someone on that network reaches the same mDNS traffic and
the same listening sockets as someone on the real one. They learn only
the random mDNS name and the protocol version. They may attempt a
handshake or a pairing request. They cannot read or write a file without
pairing first. A real pairing still needs a person to compare a code, or
scan a QR code, on both screens.

Rules for the advertisement:

- The instance name is random, not the device hostname. A default mDNS name
  broadcasts something like "Alex's Pixel" to everyone in a cafe.
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
