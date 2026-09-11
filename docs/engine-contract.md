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
   each item. The phone took its screens on 11 September 2026 and reads
   them.
4. The protocol version bumps whenever an item changes the wire in a way an
   old build cannot read; the current `VERSION_MIN` and `VERSION_MAX` live
   in `crates/ferry-core/src/version.rs`, not here. A stale build must
   fail with `NoSharedVersion` rather than read paths wrongly.
5. Nothing in the engine formats numbers, dates, or sentences. The engine
   states facts. The app formats them.

## Build order

| Batch | Items | Why this order |
|---|---|---|
| B | 1, 9, 10, 4, 8, 7, 3 | Small fields. Item 1 deletes the app's only duplicated fact. |
| C | 15, 11 | Both change the wire. One protocol bump covers both. |
| D | 2 | Batches need roots, because a batch label is a path. |
| E | 13 | The access log records paths, so it comes after roots. |
| later | 14, 12, 5, 6 | Built in the second run. See "The second run" below. |
| third | 17, 18, 19 | Phase 2 items 4, 7 and 9. See "The third run" below. |

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
| J | 12 | The engine and Mac half of QR pairing. The phone's camera screen is built. |
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

The serving side reads the file's length from the open handle first, before
any read of its bytes. The chunk size follows that length: it starts at
the one mebibyte default and steps up to the next power of two only when
needed to keep the chunk count at or under `MAX_MANIFEST_CHUNKS`. A length
above 512 GiB, the largest a manifest can describe at `ChunkSize::MAX`, is
refused with `RangeTooLarge` before any read. Otherwise the file is hashed
once with the existing `ManifestBuilder`, at the chosen chunk size. It
serves through `GuardedFs` like every other operation and is logged as a
`Stat` in the access log, because it reveals what a `stat` reveals and
nothing more. It is refused on a directory with `IsADirectory`.
`MAX_MANIFEST_BYTES` bounds the response.

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
    /// Derived, not stored: a batch with `Origin::Automatic` for this
    /// device that is not Done or Failed.
    pub running: bool,
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
records a run and makes no batch; the Running state is `running`. One
way, additive, never deletes, never writes back.

**Errors.** `set_auto_copy` on an unknown device is `Runtime::NotPaired`.

**The Mac.** The switch calls `set_auto_copy`. The section reads
`AutoCopy` and formats `last_run_unix_secs` through `FerryFormat`, and
shows the Running state exactly when `running` is true.

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
has acknowledged; the whole file is verified once at the end, so the row
moves the same way a pull's does. `push_files`
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
A `.ferry-part` name is 404 on `GET`, `HEAD`, and `PROPFIND`, and never
listed. A failed landing removes the spool file. A write on a locked
resource without its token is 423.

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
Requested { name: String, kind: DeviceKind, transport: Transport },
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
`start_pairing_with`. The phone's camera screen was built on 11 September
2026, in `screens/QrScanner.kt`; the engine half is tested engine to
engine, with one engine calling `offer_scanned` on the other's payload.

## The third run: items 17, 18 and 19

Added 11 September 2026 for the third run, after the phone took its
designed screens. These are PLAN phase 2 items 4, 7 and 9. Each engine
half is built by one builder in its own worktree. The app halves follow
once the bindings are regenerated. `Status` and `DeviceInfo` gain fields
here, so every builder runs `scripts/gate.sh mac` and `scripts/gate.sh
android` once before it reports, as rule 3 requires.

### 17. Thumbnail prefetch, PLAN phase 2 item 4: built

Finder opens a folder of 500 photos and asks for the head of every file
to draw its thumbnails. Today each of those asks costs one `Stat` and one
`Read` on the wire, four at a time through the bridge pool. After this
item a listing is followed by the bridge reading the heads itself, and a
thumbnail request then costs nothing on the wire.

**The head cache.** `crates/ferry-runtime/src/dav/heads.rs` holds the
first `HEAD_LEN = 64 KiB` of a file, keyed by the DAV target together
with the size and modified time the listing reported. A saved or replaced
file has a new size or time, so it misses on its own; nothing invalidates
by hand. The cache holds at most `HEAD_CACHE_BYTES = 32 MiB`, first in
first out. It lives on `Bridge` beside `cache`, so it dies with the mount.

**The prefetch.** After `propfind` at depth 1 has written its response,
it hands the children to one prefetch thread per bridge, started in
`MountRegistry::start` and ended by the same `running` flag. The thread
holds a queue of at most `PREFETCH_MAX_FILES = 512` targets. A new
listing replaces the queue: Finder shows one folder at a time, and the
folder a person left is not worth the wire. The thread skips a child
whose head is already cached, skips a child that is not an image by
extension, and reads `min(size, HEAD_LEN)` bytes of each other child
through the ordinary pool borrow, so it competes fairly with Finder's own
requests and can hold at most one of the four connections. One head takes
at most `limits::MAX_READS_PER_CHUNK` reads, which is 64, and the thread
reads `running` before every one of them. A peer that answers one byte per
read therefore costs 64 round trips and a short head that is dropped, not
65536 round trips, and `MountRegistry::stop` waits for one read rather
than for a whole head. The image
extensions are the constant `IMAGE_EXTENSIONS`: jpg, jpeg, png, heic,
heif, gif, webp, tif, tiff, bmp, dng, cr2, nef, arw. Compared without
case.

**Serving.** A response whose body comes entirely from the head cache may
take its size and modified time from the listing cache, when the parent
listing is within its two second TTL, and touch the wire for nothing. That
covers a thumbnail request for a prefetched file, a `HEAD`, and an empty
file. A response that needs any byte from the wire stats on the wire first,
and uses the head only when the fresh size and time match the head's key.
Otherwise it streams the whole body from the wire, as it did before item
17. So one response never carries bytes from two versions of a file: a file
replaced since the listing has a new size or time, which is a new key, so
its stale head is not used at all.

**The access log.** The prefetch of one listing is one `Read` entry on
this side through `record_this`: the folder path, the total bytes read,
and `files` set to the count. Five hundred heads are one line, and the
line is true. The phone records what it served as it does today.

**Not the transfer workers.** PLAN phase 2 item 4 said the prefetch runs
through the transfer worker pool. That pool takes transfer records and
runs whole-file pulls, and the bridge now has its own connection pool.
The prefetch uses the bridge pool. PLAN section 12 says so after this.

**Tests.** `tests/item_17_prefetch.rs`. The HTTP test client that
`tests/dav.rs` holds moves to `tests/common/mod.rs` in its own commit so
this file and `dav.rs` share it. One test lists a folder holding three
images and one text file over PROPFIND, waits until the phone's access
log shows three reads, then requests the first kilobyte of one image
with a `Range` header and proves the phone's log gains no new read. A
second test proves the head cache misses after the file is written
again with a new size. A pure test covers the extension check and the
first in first out bound. That pure test lives in `src/dav/heads.rs`,
not in the test file above, because the cache and the constants named
here are `pub(crate)` and no integration test can reach them.

### 18. Trusted networks, PLAN phase 2 item 7: built

Job 5 gets stronger. A device that paired at home stays silent in a
café, and the phone saves the battery the advertiser and the browser
spend. The app reads the Wi-Fi network name, because only the app can.
The engine decides what to do with it.

```rust
/// The app reports the name of the Wi-Fi network it is on, or None when
/// it cannot read one: Wi-Fi off, the location permission refused, or
/// the name unknown. Called after start and on every change. Idempotent.
fn set_network(&self, name: Option<String>);
/// Adds a name to the trusted list. An empty name, a name over 32
/// bytes, a name holding any control character, or a 33rd name is
/// refused with `Runtime::NetworkName`. The file holds one name per
/// line, so a name holding a newline would load as two names on the
/// next start. A stored line holding a control character is skipped
/// by `load` for the same reason.
fn trust_network(&self, name: String) -> Result<(), FerryError>;
fn forget_network(&self, name: String) -> Result<(), FerryError>;
fn trusted_networks(&self) -> Vec<String>;

// Status gains
/// The name the app last set. None when unknown.
pub network: Option<String>,
/// True while this device advertises, browses, and accepts over Wi-Fi.
/// Reports the advertiser actually running, not what the rule wants.
pub wifi_presence: bool,
```

**The rule, in two functions.** A new module,
`crates/ferry-runtime/src/networks.rs`, gains two functions, and nothing
else decides. `browse_allowed(&State) -> bool` gates the browser. It is
true when one of three things holds: the trusted list is empty; the
current network is in the list; pairing is in progress, which is any
state but `Idle`, `Confirmed`, and `Failed`. It does not read `reachable`,
because a browse query is quiet enough to run on any network.
`wifi_presence(&State) -> bool` gates the advertiser and inbound
acceptance. It is `reachable` and `browse_allowed`. The module owns the
trusted list file, both functions, and nothing else. It holds two
constants: `MAX_NETWORKS`, which is 32, bounds how many names the list
holds. `MAX_NETWORK_NAME_BYTES`, also 32, bounds the length of one name.
`State` gains the two fields the rule reads, the current network name and
the trusted list. An unknown network with a non-empty list is off for
both functions. So a person who never granted the location permission
sees no change from today, and a person who granted it once is quiet on
every network they did not pair on or trust by hand.

**Applying it.** One function, `apply_presence(shared)`, compares both
rules to what is running. It starts and stops the browser by
`browse_allowed`, and the advertiser by `wifi_presence`. Every site that
changes an input calls it: `set_reachable`, `set_network`,
`trust_network`, `forget_network`, every pairing start, every pairing
end, `start`, and `stop`. `start` calls `apply_presence` once its
listener has a port, so a wish set before `start` still takes effect.
`apply_presence` returns at once and does nothing when the engine is
stopped. `stop` turns the advertiser and the browse flag off itself, so
nothing an app calls after `stop`, such as `set_reachable(true)`, can
announce a port that is already closed.
`set_reachable` no longer starts the advertiser itself; there is one
start site and it is `apply_presence`. The browse loop keeps its thread
and drops its `Browser` while browsing is not allowed, because a browse
query is a sound on the network. The `accept_loop` welcome check refuses
a connection from a non-loopback address while Wi-Fi presence is off and
no pairing is open to an inbound handshake. Loopback is the adb tunnel,
so the cable still works on a network this device is quiet on, which is
job 2. The loopback term needs `reachable`,
not presence: `reachable` is the person's own switch, and off refuses every
connection, over the cable as well. What loopback survives is the network
half of the rule, which is the half a person never set. The pairing term is
the half of the check that was there before this item. Presence needs
`reachable`, and a Mac running the QR method never turns `reachable` on, so
dropping that term would refuse the very phone that scanned the Mac's code.

**Recording.** `finish_pairing` adds the current network to the trusted
list when it is known, on both methods and on both sides. The first
pairing at home trusts home. The list lives in `data_dir/networks`, one
name per line in UTF-8, written through the temporary name and rename
that `record.rs` uses, read at start. A missing file is an empty list.

**Reporting.** A change to `wifi_presence`, `network`, or the trusted
list fires `devices_changed`. Both apps already re-read `status()` on
that callback.

**The Mac.** `EngineModel` reads the name with CoreWLAN and hands it to
`set_network` after start and on every `ssidDidChange` event, through
`CWWiFiClient.startMonitoringEvent`, never by polling. On macOS 14 and
later CoreWLAN returns no name until the app is authorised for location,
so `project.yml` gains `NSLocationUsageDescription` and the app asks
`CLLocationManager` for authorisation the first time pairing starts. A
refusal is not an error; the name stays unknown. Settings gains a
"Networks" section: the current network with a "Trust this network"
control when it is known and not trusted, the trusted names each with a
Remove control, and one line when `reachable` is on and `wifi_presence`
is off saying Ferry is quiet on this network and why, with a control that
opens Location in System Settings when the name is unknown. The
presence control and the menu bar show the same line in that state.
Every word is a key in `Strings.swift`.

**The phone.** `Permissions` gains the fine location permission, asked
the first time pairing starts, with the reason in the request. The name
comes from a `ConnectivityManager` network callback registered for the
Wi-Fi transport with location info included, reading the `WifiInfo` from
the network capabilities; the quotes are stripped, and the unknown
placeholder is None. One object owns the callback and calls
`FerryEngine.setNetwork`. Settings gains the same "Networks" section as
the Mac and a "Location" row under Permissions. The presence row shows the
quiet-on-this-network line in that state, and the notification does not.
Every word is in `strings.xml`.

**Errors.** `Runtime::NetworkName`, one row.

**Tests.** `tests/item_18_networks.rs`. The rule as a pure function, one
case per branch. Trust, forget, and the list surviving an engine restart.
`set_network` turning `wifi_presence` off and on in `status()`. A pairing
between two engines recording the network both set. The welcome decision
as a pure function with a loopback and a non-loopback address. No test
sends a packet off this machine.

### 19. The Mac in the phone's file picker, job 8: built

PLAN phase 2 item 9. The phone's `DocumentsProvider` maps onto the file
operations layer almost one to one, so the Mac's shared folders appear
in the Files app and in every app's open dialog. It is the mirror of the
Finder mount on the same layer, and it reuses the mount's decisions.

**One pool.** The DAV bridge's per-device connection pool moves from
`dav/pool.rs` to `crates/ferry-runtime/src/pool.rs`, owned by `Shared`
as one pool per device key, made on first use and dropped by `forget`
and by `stop`. `forget` closes the pool as well as dropping it: it shuts
every idle connection down and marks the pool closed, so `take` and
`take_dialing` both answer `Runtime::NotReachable` from then on. A bridge
connection Finder already holds keeps its own handle on the pool, so
without the close its next request would read a forgotten device's files.
No pool is ever made for a device that is not paired: the pools lock is
held across that check, and `forget` holds the same lock across its own
removal and the peer list write, so a call that passed the check before
`forget` began cannot make the pool again after it. `Bridge` borrows from
it instead of owning one. The
`take`, `Borrowed`, `client`, and `mark_unhealthy` API does not change.
`Engine::list` borrows from it too, instead of dialling fresh. The
constants keep their values: four connections, and a thirty second wait
for a slot. The doc comment in `pool.rs` that says two seconds is wrong
and is corrected.

The pool gains one entry point beside `take`, because the bridge and an
engine call want different answers for a device that is not currently
marked reachable. `take` keeps item 6's rule and refuses at once, with no
dial, so Finder never beachballs. `take_dialing` dials anyway, and every
engine call below uses it, as `Engine::list` does. Without it no engine
call could reach a device that had not already been reached, because a
dial through the pool is what calls `mark_reachable` in the first place.
Both share the same four connections.

```rust
/// One entry. Refuses a path that names no file with the code the
/// wire reports, as `list` does today.
fn stat(&self, device_key_hex: String, remote_path: String) -> Result<Entry, FerryError>;
/// At most `MAX_READ_LEN` bytes, 1 MiB. A longer ask is clamped, not
/// refused: a short read is an ordinary read result.
fn read_at(&self, device_key_hex: String, remote_path: String, offset: u64, len: u32) -> Result<Vec<u8>, FerryError>;
/// Creates the file when it does not exist. More than 1 MiB in one
/// call is refused with `Runtime::WriteTooLarge`.
fn write_at(&self, device_key_hex: String, remote_path: String, offset: u64, bytes: Vec<u8>) -> Result<(), FerryError>;
fn truncate(&self, device_key_hex: String, remote_path: String, len: u64) -> Result<(), FerryError>;
fn mkdir(&self, device_key_hex: String, remote_path: String) -> Result<(), FerryError>;
/// A file, or an empty folder. The wire has no recursive delete.
fn delete(&self, device_key_hex: String, remote_path: String) -> Result<(), FerryError>;
/// Within one root, as the bridge allows.
fn rename(&self, device_key_hex: String, from: String, to: String) -> Result<(), FerryError>;
```

Every call borrows from the pool, maps wire errors exactly as `list`
and the bridge map them today, and records itself through `record_this`
with its verb, `bytes` for a read or a write, so the phone's own access
log shows what it did to the Mac, as the Mac's log shows the bridge. A
call the peer refuses records nothing: the log says what happened, and a
refused call did not happen.

**The provider.** `android/app/src/main/kotlin/app/ferry/provider/
FerryDocumentsProvider.kt`, declared in the manifest with the
`MANAGE_DOCUMENTS` permission, the `DOCUMENTS_PROVIDER` action, exported,
and URI grants, under the authority `app.ferry.documents`. A document id
is the device key hex, a slash, and the root-relative path; a root's
document id is the key hex alone, and its children are `list("")`, the
Mac's shared roots. `queryRoots` lists every paired Mac with its name as
the title and its reachability as the summary, and lists nothing while
the engine is not started, which is before all files access is granted.
`queryChildDocuments` is `list`. `queryDocument` is `stat`. `openDocument`
returns a proxy file descriptor from `StorageManager` whose callback
reads through `readAt`, writes through `writeAt`, sizes through `stat`,
and truncates to zero on open in a truncating mode, on one handler thread
the provider owns. `createDocument` is `mkdir` for a folder and a zero
byte `writeAt` for a file. `deleteDocument`, `renameDocument`, and
`isChildDocument` map by name. The Mac's shared roots carry no delete,
rename, or write flag; everything under them carries write, delete, and
rename, and folders carry create. Mime types come from the extension
through the platform table. `FerryEngine` gains one passthrough per
engine call above, and calls `notifyChange` on the roots URI from
`devicesChanged`, so the summary follows reachability. Nothing in the
provider formats English; the summary words are in `strings.xml`.

**Errors.** `Runtime::WriteTooLarge`, one row.

**Tests.** `tests/item_19_remote_ops.rs`, with the two-engine helpers
copied as every test file does today. One test walks stat, write at
zero, read back, truncate, rename, mkdir, and delete against the peer
and checks the phone-side access log holds one entry per verb with the
bytes. One test proves `list` reuses a pooled connection: two lists in a
row leave the peer's connection count at one. One test proves a
write over 1 MiB is refused with the row and writes nothing. The
provider is compile-checked only; the Files app is a manual check.
