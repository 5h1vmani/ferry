# Lifecycle and concurrency audit

Date: 11 September 2026.
Commit: 4e3e9a0 at the end of the read; the read began at 3cd5451 and the tree moved under it, so some line numbers are near, not exact.
Scope: `crates/ferry-runtime` engine lifecycle and locks, the Mac wrapper in `macos/Ferry/Engine`, and the phone wrapper under `android/app/src/main/kotlin/app/ferry`.
Method: read only, and nothing was run. Every lock site was listed with grep. Each path below was followed by hand from caller to callee.

## Lock order

Every path takes locks in this order, or takes one lock alone.

1. `mounts` (dav/mod.rs), then `pools` (`MountRegistry::start` calls `pool_for`).
2. `peers_write` (engine.rs `save_peers`), then `state`.
3. `networks_write` (engine.rs `save_networks`), then `state`.
4. `Pool::inner` (pool.rs) is dropped before `dial` takes `state`.
5. `state` is always taken alone. `set_pairing` drops it before any callback.
6. `notify.listener` is cloned and dropped before any callback runs.
7. `joins`, `sockets`, `roots`, `advertiser`, `access_log`, `auto_copy`, `auto_copy_running`, `held`, `download_fs`, `notify.outbox` are each taken alone.

No two paths take two locks in opposite orders. No inversion was found.

Locks held across slow work, which is the class audit 2 finding 13 paid for:

- `access_log` across `prune` (engine.rs `access_log_loop`, near line 3706). Finding 6.
- `auto_copy` across `save`, which calls `fsync` (auto_copy.rs 366 to 374 and 319 to 322). Finding 7.
- `mounts` across `sweep_spool`, `TcpListener::bind`, and two thread spawns (dav/mod.rs `start`). Finding 8.
- `advertiser` across `Advertiser::start` (engine.rs `apply_presence`). Found safe, it is short.

## Findings

Rank is by how often a real phone or Mac hits it. "Confirmed" means the whole path was read. "Plausible" names the step not read.

| # | Where | Sequence | Effect | Fix |
|---|-------|----------|--------|-----|
| 1 | Phone. `FerryApplication.kt` `onCreate`, `FerryDocumentsProvider.kt` lines 74 and 93, `FerryEngine.kt` `start` line 250. Confirmed. | The Files app opens a Ferry document while the Ferry activity is gone and the service is off. Android starts the process for the provider. `FerryApplication.onCreate` calls `FerryEngine.create` and never `FerryEngine.start`. Only `MainActivity.onResume` and `ReachableService.onStartCommand` call `start`. `queryRoots` returns no rows because `started.value` is false. `queryDocument` calls `stat`, which reaches `Pool::dial`, which finds `state.live` empty and returns `Runtime::NotReachable`. | The Files app shows no Ferry root, or "not reachable", every time it is used without the Ferry app in front. This is the main use of the provider. | In `FerryApplication.onCreate`, call `FerryEngine.start()` when `Permissions.allFilesAccess` is granted. The provider then sees a started engine. |
| 2 | Mac. `EngineModel.swift` `unmountAndStopBridge` line 407, called from `forget` line 588 and `stop` line 145; `dav/mod.rs` `MountRegistry::stop` line 226; `dav/server.rs` `prefetch_job`; `pool.rs` `take_inner` line 150. Confirmed. `ferry-core/src/tcp.rs` line 347 sets the read timeout to `IDLE_TIMEOUT_SECS`, 300 seconds, after every handshake, and line 424 gives the dialing side that same value. | The person clicks Forget on a mounted phone. `EngineModel` is `@MainActor`, so `mountStop` runs on the main thread. `MountRegistry::stop` sets `running` false and joins the prefetch thread. That thread checks `running` only between files. It may be inside `read_head`, blocked in a socket read on a phone that stopped answering, for up to 300 seconds. It may also be inside `Pool::take_inner`, which waits up to 30 seconds and never checks `stopping`. Nothing shuts those sockets, because `Engine::stop` is the only caller of `shutdown` on `sockets`. | The Mac window freezes for up to 30 seconds, or up to 5 minutes, on Forget or on quit. | In `MountRegistry::stop`, do not join the prefetch thread. Treat it like a connection thread, which the crate already documents as never joined. Or shut the bridge's pooled sockets before the join, the way `Engine::stop` does. |
| 3 | Phone. `ReachableService.kt` `onDestroy` line 73; `FerryEngine.kt` `stop` line 283; `transfer.rs` `dial`; `ferry-core/src/tcp.rs` line 768, `HANDSHAKE_TIMEOUT_SECS` 10. Confirmed. | The Ferry activity is closed and the person taps Stop in the notification. `Service.onDestroy` runs on the main thread. `hasActivity()` is false, so it calls `FerryEngine.stop()` there. `Engine::stop` joins every worker. A worker inside `tcp::connect` has no socket registered yet, so the `shutdown` loop cannot reach it, and `connect_timeout` runs to 10 seconds per address. `wake_the_listener` adds up to 2 seconds. | The main thread blocks for up to 12 seconds or more. Android raises an ANR for a foreground service after 20 seconds, so two dead addresses in `dial_targets` cross that line. | In `ReachableService.onDestroy`, run `FerryEngine.stop()` on a new thread, the same as `MainActivity.onDestroy` does at line 134. |
| 4 | Phone. `FerryEngine.kt` `create` line 207 and `stop` line 283, both `@Synchronized`; `MainActivity.kt` `onResume` and `onDestroy` line 132. Confirmed. | The person swipes the app away. `onDestroy` starts a thread that calls `FerryEngine.stop()`, which holds the object monitor for the whole `Engine::stop`. The person reopens the app within a few seconds. `onResume` calls `FerryEngine.create` on the main thread, which waits on the same monitor until `stop` returns. | The main thread blocks in `onResume` for as long as finding 3 takes, up to 12 seconds or more. | In `FerryEngine.stop`, take `engine` out and set the field to null under the monitor, then call `stop` on the taken engine outside it. |
| 5 | Phone. `FerryEngine.kt` `start` line 250, not synchronized, and `stop` line 283. Plausible: the step not read is whether Android can deliver `onStartCommand` while `MainActivity.onDestroy`'s stop thread is running. | `stop` is running on its thread. `ReachableService.onStartCommand` calls `FerryEngine.start()`. `engine` is still non null. `Engine::start` sees `state.started` true and returns Ok. `_started.value` becomes true. The service calls `startForeground` and `setReachable(true)`. `stop` then returns and sets `engine` null and `_started` false. | The service shows "advertising" with no engine behind it until the next `onResume`. | Make `start` `@Synchronized` too, so it waits for `stop` and then sees `engine` null and returns false. |
| 6 | Engine. `engine.rs` `access_log_loop` near line 3706. Confirmed. | Once an hour the loop takes `access_log` and calls `prune`. `prune` runs `read_dir` and `remove_file` under that lock (access.rs 740 to 751). Every served operation's `record` (guard.rs near line 109) and every bridge request's `record_this` waits on it. | Every open connection stalls for one directory scan and some unlinks, once an hour. Small on a Mac, larger on a phone with a slow flash. | Call `prune` on a clone of the store's path outside the lock, or move it to its own store handle. The `RollUp` only needs the lock for `touch`. |
| 7 | Engine. `auto_copy.rs` `record_run` lines 372 to 374 and `set_enabled` lines 319 to 322. Confirmed. | An automatic copy run ends and calls `record_run`. It holds `auto_copy` across `store.save()`, which writes a temporary file and calls `fsync` (lines 221 to 231). At the same moment a SwiftUI view calls `EngineModel.autoCopy(forDevice:)`, which calls `Engine::auto_copy` on the main actor, which takes `auto_copy`. | The Mac main thread waits for one `fsync`. This is the same class as audit 2 finding 13. | Clone the store, change and save the clone outside the lock, then swap it in, as `save_peers` does. |
| 8 | Engine. `dav/mod.rs` `MountRegistry::start`. Confirmed. | `start` holds `mounts` while it runs `sweep_spool`, which reads and deletes files, binds a port, and spawns two threads. `mount_stop` for any other device, and `stop_all`, wait on that lock. | A Forget of one phone waits for another phone's mount to finish its spool sweep. Rare and short. | Take the lock twice: once to check for an existing entry, and once to insert. Refuse a second concurrent `start` for the same key with a per key flag. |
| 9 | Phone. `FerryEngine.kt` `loadOrCreateKey`. Plausible: this needs a power loss, not a process kill. | The first run writes the key to `KEY_FILE_NAME.new` with `writeBytes` and renames it. There is no `fsync` before the rename. On ext4 and f2fs a power loss after the rename can leave a zero length file at the final name. The length check then generates a new key. | The phone gets a new identity and must pair with the Mac again. | Open the temporary file with `FileOutputStream`, call `fd.sync()`, then rename. This is the pattern `record.rs` `write_and_sync` uses. |

## Found safe

- `Engine::stop` order: flag, socket shutdown, presence, switches, roots, wake, join, roll up, bridges, pools, listener slot. Each step is read and the order is sound. The `keep` refusal after `stopping` closes the spawn race.
- `transfer::dial` checks `stopping` before every target, so no thread opens a new peer connection after `stop` returns.
- `Pool::dial` marks the device reachable and calls `notify`; after `stop` the listener slot is empty, so nothing reaches the app. The one clone race the crate documents at lib.rs line 38 is the only callback that can land after `stop`. It also covers a bridge connection thread and the `hello_with_deadline` helper thread. Both are unjoined in the same way.
- `hello_with_deadline` ticks every 50 ms and checks `stopping`, so `stop` never waits on a stalled name exchange.
- `notify_timer` is joined before `close`, so the last held report reaches the app.
- `wake_the_listener` uses a 2 second connect timeout, so a bound port that nothing accepts on cannot hang `stop`.
- `save_peers` and `save_networks` write outside `state`, with their own writer lock.
- `record.rs` and `auto_copy.rs` write a temporary file, `fsync`, then rename, so a process kill on the phone leaves whole records.
- `held.rs` appends one framed row and repairs a short tail on load, so a kill mid append loses at most that row.
- `MainActivity.onDestroy` and `ReachableService.onDestroy` together cover both orders of activity and service teardown; each stops the engine only when the other is gone.
- `FerryEngine.create` and `stop` are `@Synchronized`, so a new engine is never made while the old one still holds `DirLock`.
- `NetworkName.kt` calls `FerryEngine.setNetwork` from the connectivity callback thread, not the main thread.
- Mac `list`, `pull`, `pushFiles`, `retry`, and the engine half of `forget` run in `Task.detached`, off the main actor.
- The Mac keeps `deviceInfos`, `transferInfos`, `trustedNetworks`, and `roots` as copies. Each copy is re-read from the engine on the next callback, and `roots` is the app's own store that the engine is fed from. No copy is written to on one side only.
- `apply_presence` reads `state`, drops it, then takes `advertiser`. Two callers contend only on `advertiser`.
- `browse_loop`, `adb_loop`, and `access_log_loop` all wait in `rest`, which `stop` wakes with `notify_all`.

## Fix pass, 11 September 2026

Findings 1, 3, 4, and 9 are fixed on the phone. Finding 5 needed no
change: `FerryEngine.start` was already synchronized, and the pass says
so in a comment. Findings 2 and 8 are fixed with tests in
`tests/item_17_prefetch.rs`; a stalled peer no longer holds Forget for
minutes. Findings 6 and 7 are fixed without tests, because both are
timings a test cannot show without sleeping. The pairing sockets that
`stop` could not reach, found by the scan confirm builder, are registered
now, with a test in `tests/item_16_stop_pairing.rs`.
