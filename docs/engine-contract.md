# Engine contract for the designed screens

Adopted from the design pass of 10 September 2026. The design's own copy
was `docs-v2/contract.md`. This is the repo's version, with the sizes
corrected against the code and every open choice decided.

`crates/ferry-runtime/src/lib.rs` is the boundary. This file lists every
change the Mac screens need from it. Each item has a number. The Swift
marks every place it works around a missing item with `TODO(engine N)`.
`grep -rn "TODO(engine" macos/` is the live list of what is still missing.

Each item has a status: **open**, **built**, or **deferred**. An item moves
to built in the same commit that removes its `TODO(engine N)` markers.

## Rules for every item

1. A fact the engine reports survives an engine restart when the object it
   belongs to survives one. Transfers survive through `record.rs`, so a
   transfer's new fields are stored there too.
2. Every new error code gets a row in `design/errors.json`. The test
   `errors_have_words.rs` fails when a row is missing or a cell is empty.
   The words follow `docs/voice.md`.
3. Every boundary change regenerates both bindings with
   `scripts/gen_bindings.sh`. The Android app must still compile after
   each item. It gets no new screens in this run; a later Kotlin design
   pass will read the same fields.
4. The protocol version goes from 1 to 2 once, in item 15. Item 11 rides on
   the same bump. Both `VERSION_MIN` and `VERSION_MAX` become 2. No build
   in the field speaks version 1 with anyone but its own author, and a
   stale phone build should fail with `NoSharedVersion` rather than read
   paths wrongly.
5. Nothing in the engine formats numbers, dates, or sentences. The engine
   states facts. The app formats them.

## Build order

| Batch | Items | Why this order |
|---|---|---|
| B | 1, 9, 10, 4, 8, 7, 3 | Small fields. Item 1 deletes the app's only duplicated fact. |
| C | 15, 11 | Both change the wire. One protocol bump covers both. |
| D | 2 | Batches need roots, because a batch label is a path. |
| E | 13 | The access log records paths, so it comes after roots. |
| later | 14, 12, 5, 6 | Deferred to a later session. See the end of this file. |

## Batch B: the small fields

### 1. `status()`: built

`set_reachable(on: bool)` has no counterpart, so no presence surface can
state what is true.

```rust
#[derive(uniffi::Record)]
pub struct Status {
    pub reachable: bool,
    pub listen_port: u16,
    /// Whether adb was found when the engine started.
    pub adb_present: bool,
}

fn status(&self) -> Status;
```

Every field already exists in engine state (`state.rs`, `engine.rs`). This
item deletes `EngineModel.cachedAdvertising`, the only place the Mac app
holds a fact twice.

### 9. A transfer has a start and an end: built

```rust
// on TransferInfo
pub started_unix_secs: i64,
pub ended_unix_secs: Option<i64>,
```

`started_unix_secs` is set when `pull` creates the row. `ended_unix_secs`
is set when the state becomes `Done` or `Failed`, and cleared by `retry`.
Both are stored in the transfer record. The record format goes to version
2. A version 1 record still loads: its start time is the record file's
modification time, and its end time is `None`.

### 10. Pairing publishes its deadline: built

```rust
Waiting { expires_unix_secs: i64 },
Found { candidates: Vec<PairingCandidate>, expires_unix_secs: i64 },
Code { code: String, expires_unix_secs: i64 },
```

One deadline, stated on every state that has one. The deadline is
`now + PAIRING_TIMEOUT` at `start_pairing`. The engine already holds it as
a monotonic instant (`state.rs`); the wire needs the Unix form as well.

### 4. Direction: built

```rust
#[derive(uniffi::Enum)]
pub enum Direction { Pull, Push }

// on TransferInfo, and on BatchInfo in item 2
pub direction: Direction,
```

Always `Pull` until item 5 lands. Stored in the record with item 9.

### 8. Speed is per transfer: built

```rust
// on TransferInfo
pub speed_bytes_per_sec: Option<u64>,
```

Measured over that transfer's own bytes across the last two seconds.
`None` unless the state is `Active`. `DeviceInfo.speed_bytes_per_sec`
stays as it is.

### 7. Chunk counts: built

```rust
// on TransferInfo
pub chunks_total: u32,
pub chunks_verified: u32,
```

`chunks_total` is the chunk count of the file, known once the size is.
`chunks_verified` is how many chunks have a verified hash so far. Both are
derived on load from what the record already holds, so nothing new is
stored. `error.detail` already carries the failing chunk index.

### 3. A device lists every transport it has: built

```rust
// on DeviceInfo
pub available_transports: Vec<Transport>,
```

Holds `Usb` while an adb tunnel to the device is open, and drops it when
the forward goes away. Holds `Wifi` when a Wi-Fi connection with the
device succeeded within the last 120 seconds, in either direction, or
when `reachable_via` is `Wifi` now. Discovery alone proves nothing,
because a found address is anonymous until a handshake succeeds. `Usb`
first. `reachable_via`, when it is `Some`, is always in the list.

## Batch C: the wire

### 15. Several named shared roots: built

The largest change in this file, and a change to `ferry-core`'s file
operations layer. The design's copy said the file layer was untouched.
That was wrong: `LocalFs` serves one folder and `list("")` lists its real
contents.

```rust
#[derive(uniffi::Record)]
pub struct Root {
    /// What the peer sees as the first path segment: "Desktop".
    pub name: String,
    pub path: String,
    /// False for a root the peer may read but not write.
    pub writable: bool,
}

// on Config, replacing shared_root
pub shared_roots: Vec<Root>,
/// Where pulled files land. Never served to the peer by being here.
pub download_dir: String,

fn roots(&self) -> Vec<Root>;
fn set_roots(&self, roots: Vec<Root>) -> Result<(), FerryError>;
fn set_download_dir(&self, path: String) -> Result<(), FerryError>;
```

**Who persists.** The app does, as it already does for `display_name`.
The engine holds the live list while it runs. The app stores its roots and
its download folder in its own settings and passes them in `Config` at the
next launch. No new engine file.

**Root rules.** A name is 1 to 64 bytes of UTF-8, holds no control
character, no `/` and no `\`, and is not `.` or `..`. Names are unique
ignoring case, and a peer's first segment finds its root ignoring case.
A path must be an existing directory. There is at least one root. No two
roots share a folder or nest one inside the other, because a read-only
root inside a writable one could be written through the other name. Each
root keeps every refusal `LocalFs` already makes: symlinks, special
files, and paths that leave the folder.

**Errors.** `RootsError::RootNameInvalid`, `RootsError::RootNameTaken`,
`RootsError::RootNotAFolder`, `RootsError::RootOverlaps`,
`RootsError::NoRoots`. The runtime forwards core error names as it does
for every other core error.

**Protocol.** Every path begins with a root name. `list("")` returns one
directory entry per root: the root's name, size 0, modified time of the
folder. `stat("")` is a directory. A path whose first segment names no
root is `NotFound`. Writing to a root marked not writable is
`PermissionDenied`. Creating, deleting, or renaming a root itself is
`PermissionDenied`. Renaming across two roots is `Unsupported`.

**Where it lands.** `ferry-core` gains a `Roots` type that implements
`FileOps`, holds one `LocalFs` per root, and dispatches on the first
segment. `LocalFs` does not change. The engine opens a separate `LocalFs`
on `download_dir` for pulls, so a pull goes through the download folder's
own handle and never through a root's. If the download folder lies inside
a shared root, the peer can see and change the pulled files through that
root. Both shipped defaults do this: Downloads on the Mac and Download on
the phone. A person who shares Downloads expects that. `docs/protocol.md`
gains the root rule and the version bump.

**Defaults.** The Mac shares Desktop and Downloads, both writable, and
pulls into `~/Downloads/Ferry`. The phone shares one root named
"Internal storage" at its external storage path, writable, and its
download folder is that path's `Download` folder.

### 11. A device has a kind: built

```rust
#[derive(uniffi::Enum)]
pub enum DeviceKind { Phone, Mac }

// on Config: what this device is
pub kind: DeviceKind,

// on DeviceInfo: what the peer said in hello
pub kind: DeviceKind,
```

The hello payload gains one kind byte after the name. Protocol version 2.
The peer store format goes to version 2 and stores the kind. A version 1
peer file still loads: every peer in it gets the opposite of this
device's `Config.kind`, because with one Mac and one phone that is always
right.

## Batch D: batches

### 2. A batch is not an object: built

`TransferInfo` is one file. Without a batch, pulling a camera folder
produces 120 rows. Nothing in the engine correlates pulls today, and the
design's copy gave no way to create a batch. This does.

```rust
/// Why a batch exists.
#[derive(uniffi::Enum)]
pub enum Origin {
    /// A person asked for it.
    Manual,
    /// Ferry decided, under item 14's rule. Never emitted until item 14.
    Automatic,
}

#[derive(uniffi::Record)]
pub struct BatchInfo {
    pub id: String,
    pub device_key_hex: String,
    /// The remote path as given: "Internal storage/DCIM/Camera".
    pub label: String,
    pub files_total: u32,
    pub files_done: u32,
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// The worst state among its transfers: Failed, then Paused, then
    /// Active, then Queued, then Done.
    pub state: TransferState,
    pub direction: Direction,
    pub origin: Origin,
    /// The sum over its active transfers.
    pub speed_bytes_per_sec: Option<u64>,
    pub started_unix_secs: i64,
    pub ended_unix_secs: Option<i64>,
    /// The transport of any active transfer in the batch.
    pub transport: Option<Transport>,
    /// The error of the first failed transfer, so the row can say why.
    pub error: Option<FerryError>,
}

// on TransferInfo
pub batch_id: Option<String>,

/// Retries every failed transfer in the batch.
fn retry_batch(&self, batch_id: String) -> Result<(), FerryError>;

/// Copies a whole folder. Lists it over the connection, then queues one
/// transfer per file under the batch. Blocks until the listing is done,
/// so the app calls it off the main thread as it does `list`.
fn pull_folder(&self, device_key_hex: String, remote_path: String)
    -> Result<String, FerryError>;

fn batches(&self) -> Vec<BatchInfo>;
```

**Rules.** Files land under `download_dir/<last segment of remote_path>/`
with their relative paths kept. A listing stops at 10,000 files or a
depth of 32 with `Runtime::FolderTooLarge`. An empty folder makes a batch
with zero files in state `Done`. `retry` keeps a transfer's batch. A
single-file `pull` has no batch. `forget` removes the device's batches
with its transfers. A batch is stored under `data_dir/batches/` so a
restart keeps the grouping. `transfers_changed` covers batches; the app
reads `batches()` after it. No new listener method.

**The Mac.** The Files section gains "Copy to Mac" on a folder row, which
calls `pull_folder`. This is the one screen change outside the design.
Without it the engine feature has no caller. A failed batch row shows the
error and a retry control, the same as a failed single transfer.

## Batch E: the access log

### 13. The access log: built

Job 9 in `docs/jobs.md`. Logging happens at the file operations layer,
not the transfer engine. A Finder browse or a file picker open is `list`,
`stat`, and `read` and produces no transfer.

```rust
#[derive(uniffi::Enum)]
pub enum AccessVerb { List, Stat, Read, Write, Truncate, Rename, Mkdir, Delete }

/// Who performed the operation.
#[derive(uniffi::Enum)]
pub enum Actor {
    /// The peer, on this device's files.
    Peer,
    /// This device, on the peer's files.
    This,
}

#[derive(uniffi::Record)]
pub struct AccessEntry {
    /// "<day>-<sequence>". Stable across restarts.
    pub id: String,
    pub device_key_hex: String,
    pub actor: Actor,
    pub verb: AccessVerb,
    /// Root-relative, beginning with the root name: "Desktop/Q3 notes.md".
    pub path: String,
    pub bytes: Option<u64>,
    /// For a list, how many entries were returned.
    pub entries: Option<u32>,
    /// For a folder copy, how many files it covered.
    pub files: Option<u32>,
    pub at_unix_secs: i64,
}

/// Newest first. None for `device_key_hex` returns every device's.
/// `limit` is capped at 1,000.
fn access_log(&self, device_key_hex: Option<String>, limit: u32) -> Vec<AccessEntry>;

// on EngineListener
/// At most once every 250 milliseconds. The app then calls `access_log`.
fn access_log_changed(&self);
```

**Where it records.** On the serving side, in `GuardedFs` (`guard.rs`),
as actor `Peer`. On the calling side, in the engine's `list`, `pull`, and
`pull_folder`, as actor `This`.

**Rolling up.** One entry per connection, verb, and path. Bytes add up
while operations continue. An entry is final when the same connection
touches a different path, when five seconds pass without a new operation
on it, or when the connection ends. The listener fires for final entries
only. A `list` entry counts every page of the listing. A `pull_folder`
records one entry with the file count and the byte total on the calling
side. A file copied as part of a folder copy logs nothing of its own on the
calling side; the folder entry covers it. `set_mtime` is not logged: it
always follows a write that is.

**Storage.** `data_dir/access_log/<YYYYMMDD>`, one file per UTC day, a
version byte first, the same encoder as the peer store. A day holds at
most 10,000 entries; after that the day records nothing more. Files older
than 30 days are deleted at start and once an hour. `forget` keeps the
device's entries: a log that erases the record of the device you just
distrusted is not a log.

**Thirty days.** Stated on screen, because a log that quietly forgets is
worse than no log.

## The second run: items 16, 14, 5, 6 and 12

Decided 11 September 2026 after three research passes over the engine,
the mount spike, and the two screens. The order below is the build order.
Each item states its decisions so that a builder needs no other source.

| Batch | Items | Why this order |
|---|---|---|
| F | 16 | The manifest request is what items 14, 5 and 6 verify with. |
| I1 | 6, read-only | Needs nothing from F, so it runs beside it. |
| G | 14 | Reuses `pull_folder`, batches, and the manifest request. |
| H | 5 | Push, with its own Mac control, so it is used before the mount needs it. |
| J | 12 | The engine and Mac half of QR pairing. The phone's camera screen waits for the Kotlin design pass. |
| I2 | 6, the write verbs and the delta on save | Needs push and the manifest request. |

### 16. The runtime limits: built

Three engine changes from `PLAN.md` phase 2, item 1. None changes an
existing frame, so the protocol version stays at 2 unless item 12 moves it.

**16a. A manifest request.** A new operation in the file operations layer:

```rust
// ops.rs
Request::Manifest { path }
Response::Manifest { manifest }   // the encoded Manifest: length, chunk size, chaining values, root
```

The serving side reads the whole file once and hashes it with the
existing `ManifestBuilder`. It serves through `GuardedFs` like every other
operation and is logged as a `Stat` in the access log, because it reveals
what a `stat` reveals and nothing more. It is refused on a directory with
`IsADirectory`. `MAX_MANIFEST_BYTES` bounds the response.

Who uses it: a pull's first pass fetches the manifest before the first
chunk and verifies every chunk as it lands, so the first pass is no longer
trusted. A verified first pass changes the resume sweep's wire arithmetic
by one request and one response per pull; the sweep's prediction changes
with it. Items 5, 6 and 14 use it as stated there.

Not built: a "do you hold root hash X" request. Nothing in items 5, 6 or
14 needs it. The Mac answers that question from its own held index (item
14), and the delta on save (item 6) compares manifests.

**16b. The responder tries each stored key.** Today `choose_peer` picks
one stored peer by address and otherwise the first one, so a second paired
phone can fail its handshake. The responder reads the first `KK` message
once, then tries each stored key, the one whose last address matches
first, and binds the first that authenticates. `MAX_PEERS` bounds the
trials. `noise.rs` gains the split between receiving message one and
binding a candidate; `tcp.rs` passes the candidates. Test: two paired
peers, no address hint, both connect.

**16c. `stop` closes every socket.** A worker blocked in a kernel read
cannot see the stopping flag, so `stop` can wait up to the idle timeout.
Every connection registers a clone of its `TcpStream` under its
connection id when it is established, and removes it when it ends. `stop`
calls `shutdown(Both)` on each after setting the flag. The clone lives in
`Shared`, keyed by the same connection id the access log uses. Test: a
peer that accepts and never answers; `stop` returns within two seconds.

### 14. Automatic copying, job 7: built

```rust
#[derive(uniffi::Record)]
pub struct AutoCopy {
    pub device_key_hex: String,
    pub enabled: bool,
    /// The peer folder watched, root-relative: "Internal storage/DCIM".
    pub source: String,
    /// Where copies land: "<download_dir>/DCIM".
    pub destination: String,
    pub last_run_unix_secs: Option<i64>,
    pub last_run_files: Option<u32>,
}

fn auto_copy(&self, device_key_hex: String) -> AutoCopy;
fn set_auto_copy(&self, device_key_hex: String, enabled: bool) -> Result<(), FerryError>;
```

**Storage.** One file, `data_dir/auto_copy`, keyed by device key, in the
peer store's shape: a version byte, a bounded count, temp then rename.
It holds `enabled`, `last_run_unix_secs`, `last_run_files`. `source` is
the device's first root followed by `/DCIM`, and `destination` is the
download folder followed by `/DCIM`; neither is stored.

**The held index.** `data_dir/held`, a second file in the same shape. One
row per file this device has pulled to completion: the device key, the
source path, its size and modified time at pull time, and the root hash.
Every completed pull writes a row, manual or automatic. The index is how
the Mac knows what it holds. A file the person later deletes from the
download folder stays held, because job 7 says "never copied twice".

**The run.** A run starts when a device with `enabled` becomes reachable
(the transition from not reachable to reachable, detected in
`mark_reachable`), when `set_auto_copy(true)` is called on a reachable
device, and at `start` for every reachable enabled device. One run per
device at a time. A run lists `source` recursively with the same bounds
as `pull_folder`. A file is skipped when the index holds its device, path,
size and modified time. For the rest, the run asks the peer for the
file's manifest and skips the file when the index holds its root hash
under any path. What remains is queued as one batch with
`Origin::Automatic`, labelled with the source path. When the batch ends,
`last_run_unix_secs` is the end time and `last_run_files` is the number of
files it copied, zero when nothing was new. A run that finds nothing new
still records a run. One way, additive, never deletes, never writes back.

**Errors.** `set_auto_copy` on an unknown device is `Runtime::NotPaired`.

**The Mac.** The switch calls `set_auto_copy`. The section reads
`AutoCopy` and formats `last_run_unix_secs` through `FerryFormat`. The
Running state is derived: a batch with `Origin::Automatic` for that
device that is not Done or Failed.

### 5. Push: built

```rust
/// One file. `local_path` is absolute on this device. `remote_path` is
/// root-relative on the peer and names the file, not its folder.
fn push(&self, device_key_hex: String, local_path: String, remote_path: String)
    -> Result<String, FerryError>;

/// Several files into one peer folder, as one batch labelled with the
/// folder. Returns the batch id.
fn push_files(&self, device_key_hex: String, local_paths: Vec<String>, remote_folder: String)
    -> Result<String, FerryError>;
```

**Who does what.** Every push decision is the sender's. The peer serves
`write`, `truncate`, `rename`, `set_mtime`, and the manifest request, and
its access log records the writes. The sender opens the local file through
a `LocalFs` on its parent folder, so the same refusals apply as
everywhere else: no symlink, no special file. The sender builds the
file's manifest first, then writes chunks to `<remote_path>.ferry-part`
at their offsets. On resume it asks the peer for the manifest of the
partial file, compares chaining values, and continues from the first
chunk that differs. When every chunk is written it asks once more; if the
root hash matches it renames the partial to `remote_path` and sets the
modified time, otherwise it restarts from the first mismatch. A partial
the sender never finishes stays on the peer with its `.ferry-part` name.
The receiver's `writable` flag on the root is the only permission check.

**Rows and batches.** `Direction::Push` on the transfer, `source` the
local path, `destination` the remote path. `bytes_done` is bytes the peer
has verified, so the row moves the same way a pull's does. `push_files`
makes a batch with `Direction::Push`, `Origin::Manual`, label
`remote_folder`. A push is stored, resumed and retried like a pull.

**Tests.** The happy path in `two_engines.rs`. In `engine_paths.rs`, a cut
at three points of a push resumes and rewrites at most one chunk. No full
sweep for push: the resume rule is the same code path as the pull sweep
already proves, and the sweep is the most expensive test in the suite.

**The Mac.** The Files section gains "Copy to phone" beside the folder
controls. It opens the file panel for one or more files and calls
`push_files` into the folder the section is showing. One string added.
Rows show the direction through the existing `TransferRow`.

### 6. The mount: built

The Finder mount is a WebDAV bridge, as ADR 0008 decided. The bridge
lives in the Rust runtime as `crates/ferry-runtime/src/dav/`, a
hand-written blocking HTTP/1.1 server with no new dependency, as the
spike was. One accept thread, one thread per connection, as every other
transport in this engine. The Mac app performs the mount through the
NetFS API with the credentials passed in memory, never on a command line.

```rust
#[derive(uniffi::Record)]
pub struct MountEndpoint {
    /// "http://127.0.0.1:<port>/". Loopback only.
    pub url: String,
    pub user: String,
    /// Random per start. Never shown.
    pub password: String,
}

/// Starts serving the device's roots over WebDAV. Idempotent.
fn mount_start(&self, device_key_hex: String) -> Result<MountEndpoint, FerryError>;
fn mount_stop(&self, device_key_hex: String);
/// The app reports where the OS mounted it, or None when it unmounted.
fn set_mount_path(&self, device_key_hex: String, path: Option<String>) -> Result<(), FerryError>;

// on DeviceInfo, replacing Status.mount
pub mount_path: Option<String>,
```

`Status.mount` from item 1 is removed: one fact, one place, and the fact
is per device.

**I1, browsing.** `OPTIONS`; `PROPFIND` at depth 0 and 1 with
`resourcetype`, `getcontentlength`, `getlastmodified`, `getetag`,
`creationdate` and `displayname`, whatever the request body asks for;
`GET` and `HEAD` with one `Range`, served in bounded pieces so a
disconnect is seen at the next write; `ETag` is size and modified time.
The root of the mount lists the device's roots, which is `list("")`.

The bridge answers Apple's metadata probes locally and never puts them on
the wire: any last segment beginning with `._`, and the names `.DS_Store`,
`.hidden`, `.Spotlight-V100`, `.metadata_never_index`, `.Trashes`,
`.fseventsd`, `.TemporaryItems`, and anything beginning with `.ql_`. A
read of one is 404; a write of one is accepted into a local sidecar store
under `data_dir/dav_sidecars/` so Finder is content and the phone never
sees it. A listing never shows a sidecar.

A depth 1 listing is cached for two seconds per folder, and dropped by any
write through the bridge to that folder. Authentication is Basic with a
constant-time compare; anything else is 401. The `Host` header must be
`127.0.0.1:<port>` or the answer is 400. The listener binds `127.0.0.1`
on a random port. The bridge talks to the phone through a small pool of
the engine's own connections, so one stalled Finder request does not hold
every other one.

**I2, saving.** `PUT` receives the whole body into a spool file under
`data_dir/dav_spool/`, builds its manifest, then: for a new file, writes
every chunk to `<name>.ferry-part` and renames, the push rule from item
5; for an existing file, asks the peer for its manifest and writes only
the chunks that differ, in place, then truncates to the new length. That
is the delta on save. `LOCK` and `UNLOCK` keep an in-memory lock table
with tokens and a timeout; `PUT`, `DELETE`, `MOVE`, `MKCOL` and
`PROPPATCH` honour the `If` header against it. `MKCOL` is `mkdir`.
`DELETE` on a folder walks it with the folder bounds and deletes leaves
first, because Finder expects it; the wire stays non-recursive. `MOVE` is
`rename` within one root; across roots it is 502. `COPY` of a file reads
and writes through the bridge; `COPY` of a folder is 403. `PROPPATCH`
sets the modified time when asked and answers 403 per property otherwise.

**Lifecycle.** The Mac app starts the bridge and mounts when a phone
becomes reachable, names the volume after the device, and reports the
path. It unmounts and stops the bridge at quit and at `forget`. While the
phone is not reachable, the bridge answers 503 at once rather than
hanging, so Finder shows an error instead of a beachball. `stop` stops
every bridge.

**Errors.** `Runtime::MountFailed`, with detail from the OS or the bridge.

**Tests.** A hand-written HTTP client in the test file, against two
engines: every verb above, the delta writing only changed chunks (count
the peer's `write` entries in its access log), probes never reaching the
peer, auth and `Host` refusals, the cache, and the lock table. Nothing
automated proves Finder itself mounts and lists; the spike proved the
route and a person checks the rest with `docs/manual-checks.md`.

### 12. QR pairing: built

The scan method proves possession earlier than six digits: the phone
learns the Mac's static key from the screen, so the handshake is `IK` and
no wrong confirm can accept a stranger.

```rust
#[derive(uniffi::Record)]
pub struct PairingOffer {
    /// ASCII: "FERRY1:" then base64url of version(1), the Mac's static
    /// public key(32), expiry(8), nonce(16), then addresses as count(1)
    /// and ip(16 or 4 with a tag) and port(2) each. Drawn as a QR code.
    pub payload: Vec<u8>,
    pub expires_unix_secs: i64,
}

#[derive(uniffi::Enum)]
pub enum PairingMethod { Code, Qr }

/// Replaces `start_pairing`.
fn start_pairing_with(&self, method: PairingMethod);
/// Phone only. The bytes its camera decoded.
fn offer_scanned(&self, payload: Vec<u8>) -> Result<(), FerryError>;

// PairingState gains
Offering { offer: PairingOffer },
Requested { name: String, transport: Transport },
```

**Flow.** `start_pairing_with(Qr)` on the Mac makes a nonce and a two
minute expiry, publishes `Offering`, and accepts `IK` handshakes only
while offering and only when message one carries the current nonce. The
phone's `offer_scanned` parses the payload, refuses a non-Ferry payload,
an expired one, or a key it already holds, dials the addresses in order,
and runs `IK` as the initiator with the nonce and its hello in message
one. The Mac then shows `Requested` with the phone's name and transport;
`confirm_pairing(accept:)` answers it as it answers `Code`. The phone asks
no second question: the scan was its answer. The nonce is single use and
dies with the offer, so a photographed screen is useless two minutes
later. Wi-Fi addresses only: over the cable the phone cannot reach the
Mac. The code method stays exactly as it is.

**Protocol.** `docs/protocol.md` section 4 gains the scan method and the
`IK` pattern. If the pre-handshake exchange cannot say which pattern
follows, it gains a mode byte and the version moves to 3, both apps
rebuilt together.

**Errors.** `PairingError::OfferExpired`, `PairingError::OfferNotFerry`,
`PairingError::AlreadyPaired`, and `PairingError::CameraRefused`, the last
one the app's, all with rows.

**The Mac.** `PairingQRView` draws the real payload; `isReal` goes away.
The adapter maps `Offering` and `Requested`. `startPairing(method:)` calls
`start_pairing_with`. The phone's camera screen is not built now; the
engine half is tested engine to engine, with one engine calling
`offer_scanned` on the other's payload.
