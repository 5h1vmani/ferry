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

### 1. `status()` — built

`set_reachable(on: bool)` has no counterpart, so no presence surface can
state what is true.

```rust
#[derive(uniffi::Record)]
pub struct Status {
    pub reachable: bool,
    pub listen_port: u16,
    /// Whether adb was found when the engine started.
    pub adb_present: bool,
    /// Where the peer's roots are mounted on this device. None until
    /// item 6 is built.
    pub mount: Option<String>,
}

fn status(&self) -> Status;
```

Every field already exists in engine state (`state.rs`, `engine.rs`). This
item deletes `EngineModel.cachedAdvertising`, the only place the Mac app
holds a fact twice.

### 9. A transfer has a start and an end — built

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

### 10. Pairing publishes its deadline — built

```rust
Waiting { expires_unix_secs: i64 },
Found { candidates: Vec<PairingCandidate>, expires_unix_secs: i64 },
Code { code: String, expires_unix_secs: i64 },
```

One deadline, stated on every state that has one. The deadline is
`now + PAIRING_TIMEOUT` at `start_pairing`. The engine already holds it as
a monotonic instant (`state.rs`); the wire needs the Unix form as well.

### 4. Direction — built

```rust
#[derive(uniffi::Enum)]
pub enum Direction { Pull, Push }

// on TransferInfo, and on BatchInfo in item 2
pub direction: Direction,
```

Always `Pull` until item 5 lands. Stored in the record with item 9.

### 8. Speed is per transfer — built

```rust
// on TransferInfo
pub speed_bytes_per_sec: Option<u64>,
```

Measured over that transfer's own bytes across the last two seconds.
`None` unless the state is `Active`. `DeviceInfo.speed_bytes_per_sec`
stays as it is.

### 7. Chunk counts — built

```rust
// on TransferInfo
pub chunks_total: u32,
pub chunks_verified: u32,
```

`chunks_total` is the chunk count of the file, known once the size is.
`chunks_verified` is how many chunks have a verified hash so far. Both are
derived on load from what the record already holds, so nothing new is
stored. `error.detail` already carries the failing chunk index.

### 3. A device lists every transport it has — built

```rust
// on DeviceInfo
pub available_transports: Vec<Transport>,
```

Holds `Usb` when an adb tunnel to the device is open. Holds `Wifi` when
the device was seen by discovery within the discovery lifetime. `Usb`
first. `reachable_via`, when it is `Some`, is always in the list.

## Batch C: the wire

### 15. Several named shared roots — built

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
on `download_dir` for pulls, so a pull never writes into a served root.
`docs/protocol.md` gains the root rule and the version bump.

**Defaults.** The Mac shares Desktop and Downloads, both writable, and
pulls into `~/Downloads/Ferry`. The phone shares one root named
"Internal storage" at its external storage path, writable, and its
download folder is that path's `Download` folder.

### 11. A device has a kind — built

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

### 2. A batch is not an object — open

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
}

// on TransferInfo
pub batch_id: Option<String>,

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
Without it the engine feature has no caller.

## Batch E: the access log

### 13. The access log — open

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
side. `set_mtime` is not logged: it always follows a write that is.

**Storage.** `data_dir/access_log/<YYYYMMDD>`, one file per UTC day, a
version byte first, the same encoder as the peer store. A day holds at
most 10,000 entries; after that the day records nothing more. Files older
than 30 days are deleted at start and once an hour. `forget` keeps the
device's entries: a log that erases the record of the device you just
distrusted is not a log.

**Thirty days.** Stated on screen, because a log that quietly forgets is
worse than no log.

## Deferred to a later session

These four change what Ferry is, not what it says. The screens render
each one as honestly unavailable today.

### 14. Automatic copying, job 7 — deferred

`PLAN.md` phase 2, item 2. Depends on the manifest request and content
skip in phase 2, item 1. The design's `AutoCopy` record and `auto_copy` /
`set_auto_copy` calls stand as written in the design's copy.

### 12. QR pairing — deferred

`PLAN.md` phase 3. A protocol change to `IK`, a camera permission on the
phone, and four error rows. The design's `PairingOffer`,
`PairingMethod`, `start_pairing_with`, `offer_scanned`, and the
`Offering` and `Requested` states stand as written in the design's copy.

### 5. Push — deferred

`PLAN.md` phase 2, item 5. `Direction::Push` exists from item 4 so that
nothing is renamed when push lands.

### 6. Mount state — deferred

`PLAN.md` phase 2, item 3. The choice the design left open is already
settled by ADR 0008: the Mac side owns the WebDAV bridge, so
`Status.mount` from item 1 is where the path will be reported.
