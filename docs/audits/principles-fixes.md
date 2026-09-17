# The principles fix pass audit

Date: 17 September 2026
Commit: 1cddcfb, the head of main. The range read is `2be6a97..1cddcfb`, 57
commits.
Scope: eight areas of the principles fix batch. They are the shared on-disk
write module (3366e80), the shared wire helpers (ec07c6e), the new
`landing_folder` engine call (b85d899), the Mac threading split (90b8862 and
ca140a2), the phone threading and lifecycle split (c8def97, d011416, 5caf6a7,
cfe6cf3, 91d02c3, ac64e55), the adb timeout change (94e8a8e), the test
changes (513bf60, e123c20), and the CI workflow (c0b42b4, b570303). The
review that drove the batch is `docs/audits/principles.md`.
Method: read only. No build, no test, no app run. Each area was read at
1cddcfb and compared with the same file at 2be6a97, using `git diff` and
`git show`. "Confirmed" means the whole path was read in the repo.
"Plausible" names the step that was not read.

Note on the brief: the adb change is commit 94e8a8e, not 1cddcfb. Commit
1cddcfb only adds three lines to `.gitignore`.

## Findings

| No. | Area | Where (file:line) | What happens | Fix | Severity |
|---|---|---|---|---|---|
| 1 | Mac threading | `macos/Ferry/Engine/EngineModel.swift:141`, `:145`, `:180-194`; callers `macos/Ferry/FerryApp.swift:28` and `macos/Ferry/ContentView.swift:27` | `start()` is now async. The guard `guard engine == nil` runs at `:141`, then the code suspends at the `await` on `:145`. `engine` is not set until `:180`. So a second call passes the same guard while the first is still reading disk and the Keychain. Two callers exist: the window's `.task` at `FerryApp.swift:28`, which runs again for a second window on the same shared `@StateObject`, and the Retry control at `ContentView.swift:27`. The second `Engine` is refused, because `Engine::new` takes an exclusive `flock` on the data directory (`crates/ferry-runtime/src/engine/api/lifecycle.rs:116`). The loser's catch at `:189-194` then sets `engine = nil` and `events = nil`. Those are the winner's fields. The running first engine loses its only strong reference and is freed without `stop()`. The person is left on the start error screen, and every later gesture is a silent no-op, because every method starts with `guard let engine`. Before 90b8862 `start()` was synchronous and the guard held. Confirmed. | Set a `starting` flag on the main actor before the `await`, and clear it in both exits. Make the catch restore nothing it did not set, by building into locals and publishing once. | high |
| 2 | Landing folder | `crates/ferry-runtime/src/engine/api/transfers.rs:452-456`; caller `macos/Ferry/Screens/DeviceDetail.swift:85-87` and `:138-145` | `Engine::landing_folder` always calls `mkdir` on the peer. The Mac's detail pane calls it from `.task(id: landingFolderReloadKey)` just to draw a caption. So opening a device's detail pane, or the device flipping reachable, now creates `Downloads/Ferry` on the other person's disk. Nothing was sent. Before the batch the caption path called `list("")` only and built the name locally (`2be6a97:macos/Ferry/Engine/EngineModel.swift:688-696`); only `send` did the `mkdir`. Commit 1cc324c also replaced `try?` with a catch that sets `model.actionError`, so a read-only root now shows an error block from viewing alone. The call also writes two access log rows on the peer per view, a List and a Mkdir, instead of one. Confirmed. | Split the call in two. Give the caption a read-only form that lists and names the folder, and keep the `mkdir` inside the push path in `EngineModel+Sending.swift:88`. | medium |
| 3 | Landing folder | `crates/ferry-runtime/src/engine/api/transfers.rs:428`; words at `macos/Ferry/Generated/Errors.swift:67` and `android/app/src/main/kotlin/app/ferry/Errors.kt:70` | An empty root list from the peer returns `RootsError::NoRoots`. That code's words are "Nothing is shared. The list of shared folders is empty. Add at least one folder." Those words describe this device's own setup. Here the empty list belongs to the peer. A person who follows the todo adds a folder on the wrong device, and the push still fails. `FerryEngineShare.kt:63-66` states in a comment that every code already has words, which is true, but the words are not true of this case. This repeats `docs/audits/ux-gestures.md` finding 8 in a new place. Confirmed. | Add a code for "the other device shares nothing", with its own words naming the peer, and return that from `landing_folder`. | medium |
| 4 | Mac threading | `macos/Ferry/Engine/EngineModel+Mounting.swift:20-30` and `:37-47` | `clearEjectedMounts` collects `(keyHex, path)` pairs at `:21-23`, checks the disk on a detached task at `:26`, then publishes only the keys at `:28`. `clearMountPaths` matches on key alone at `:39` and clears without re-checking the path. So a device that remounts at a different path between the check and the publish loses a live mount. `FinderMount.mount` can land at the OS default location when the named directory fails, so a different path is real. The device cannot be mounted again, because its key stays in `mountAttempted` until it stops being reachable (`:62`, `:69`). The window is narrow, because `fileExists` is fast and mounting is slow. Confirmed. | Pass the pairs through to `clearMountPaths`, and clear only where the stored `mountPath` still equals the path that was tested. | medium |
| 5 | Phone lifecycle | `android/app/src/main/kotlin/app/ferry/ReachableService.kt:58`, `:71-75`, `:151`; `android/app/src/main/kotlin/app/ferry/TransferNotifier.kt:42-66` | `onDestroy` calls `notifyJob?.cancel()` at `:151` and does not join. `updateTransferNotifications` has no suspension point between `:42` and `:66`, so a pass already running on a `Dispatchers.Default` thread finishes after the cancel returns. `onDestroy` runs on the main thread, so it can return while `manager.notify` at `TransferNotifier.kt:57` still runs. The posted row is ongoing (`:90`, `:96`, `:102`), and no instance is left to cancel it. `cancelStaleTransferNotifications` runs only in `onCreate` at `:70`, so the row sits in the shade until the service starts again. `notifyScope` at `:58` is never cancelled anywhere in the tree. The split in d011416 moved this code without changing it, so the gap predates the split. Confirmed. | Cancel `notifyScope` in `onDestroy`, not just the job, and cancel the transfers channel there as well as in `onCreate`. | medium |
| 6 | Phone lifecycle | `android/app/src/main/kotlin/app/ferry/ShareCacheRegistry.kt:58-71` and `:77-85`; `android/app/src/main/kotlin/app/ferry/ShareIntake.kt:127-131` | The sweep order after the split is correct. `_batches` is written at `FerryEngine.kt:287`, before `_started` at `:299`, and the sweep waits on `started` at `ShareIntake.kt:129`. But the sweep still deletes files a live share needs. `registerBatch` writes the entry under a request UUID (`FerryEngineShare.kt:55-58`). In `sweepOrphaned`, `known[entry.name]` is null for that UUID, so `:64` treats it as orphaned and calls `deleteRegistered`. That removes the cache copy while `pushFiles` at `FerryEngineShare.kt:68` is reading it. `deleteUnnamedCacheFolders` at `:77-85` also removes any cache folder no surviving entry names, which covers a copy made but not yet registered. The race is reachable, because `ShareIntake.start` runs at `FerryApplication.kt:27` while a share intent can already be copying. The same code is present at 2be6a97, so this predates the batch. Confirmed. | Keep request-id entries out of the orphan rule. Store a written time in the entry and spare anything younger than a stated age, or mark request-id entries with a prefix the sweep skips. | medium |
| 7 | adb timeout | `crates/ferry-core/src/adb.rs:305-316`, `:371-391`; poll at `crates/ferry-runtime/src/engine/loops.rs:74-79`; interval at `crates/ferry-runtime/src/engine.rs:108` | After a timeout the two reader `JoinHandle`s are dropped, not joined. A dropped handle detaches the thread. Each thread stays blocked in `pipe.read` until the write end closes. If a process `adb` started still holds that write end, the thread and its pipe descriptor stay for the life of Ferry. Each leaked thread can also hold up to 1 MiB of captured bytes (`MAX_CAPTURED_OUTPUT`, `:364`). The poll runs every 3 seconds and the default timeout is 10 seconds, so a permanently hung adb gives about 277 timed-out calls an hour, about 554 threads and 1108 descriptors. The direct child is killed and reaped at `:350-351`, so there is no zombie. Confirmed in Ferry's own code. Plausible, not confirmed, for the real adb: I did not read the adb source, so I cannot say whether the adb server it forks keeps the client's stdout and stderr pipes, or redirects them to its own log file at start-up. The tests at `:548` and `:565` prove the shape with a shell, not with adb. | Give the child a temporary file for stdout and stderr instead of a pipe. Then no reader thread exists, and a grandchild holding the file blocks nothing. | medium |
| 8 | Phone lifecycle | `android/app/src/main/kotlin/app/ferry/ReachableService.kt:268`; `android/app/src/main/kotlin/app/ferry/TransferNotifier.kt:55` | `postRetryFailed` posts on `transferNotifier.notificationIdFor(notificationGroupId)`, the same id the group's own row uses. It does not update `lastNotified`. The next collector pass sees the group's state unchanged, computes `changed == false` at `TransferNotifier.kt:55`, and posts nothing. So the group's real row is never drawn over the retry-failure text. Confirmed. | Give the retry failure its own notification id, or clear the group's entry from `lastNotified` when the retry failure is posted. | medium |
| 9 | Mac threading | `macos/Ferry/Engine/EngineModel.swift:44`, `:46`, `:48`, `:50`, `:61`, `:64` | Six `@Published` properties lost `private(set)` in the split: `devices`, `pairing`, `presence`, `roots`, `downloadPath`, and `trustedNetworks`. They all had it at `ca140a2^:macos/Ferry/Engine/EngineModel.swift:38-54`. Their setters are now internal, and every SwiftUI view holds the model through `@EnvironmentObject`. No view writes any of the six today, checked by grep across `macos/Ferry`. The commit message does not mention the change. Confirmed. | Restore `private(set)` and mark the six extension files' writers as the only writers. Swift allows `private(set)` with writes from an extension in the same module. | low |
| 10 | Mac threading | `macos/Ferry/Engine/EngineModel+Transfers.swift:46-51`; cleared at `macos/Ferry/Engine/EngineModel.swift:225` | `storeRevealPaths` writes a key for each finished transfer and never removes one. `revealPaths` is cleared only in `stop()`. A transfer that `forget` removed keeps its entry for the life of the run. Nothing reads a stale key, because the consumer at `EngineAdapter.swift:78` looks up only ids in the current list. The dictionary only grows. Confirmed. | Drop keys that no current transfer names, in the same pass that stores new ones. | low |
| 11 | Tests | `crates/ferry-runtime/tests/common/mod.rs:31` versus `crates/ferry-runtime/tests/common/paths.rs:37` | The poll helper consolidation changed two files' budgets. `item_17_prefetch.rs` and `item_6_spool_count.rs` used `common::PATIENCE`, which is 10 seconds. They now call `common::paths::poll_until`, whose `PATIENCE` is 20 seconds. So each wait is twice as patient as before. The tick also changed in the prefetch file, from 20 ms to 10 ms. The commit message says the loop is the one the helpers already name, which is true of the loop but not of the budget. Confirmed. | State the change in the commit note, or pass the file's own patience through `poll_every`, which already takes it. | low |
| 12 | Tests | `crates/ferry-runtime/tests/item_6_spool_count.rs:64-65` | `wait_for_count_to_match_disk` polls until the two counts match, then reads both again in the `assert_eq!`. The old form asserted on the same readings that satisfied the poll, so it could never fail. The new form can fail when the spool changes between the poll and the assert. The helper is called right after a `PUT` lands, at `:142`, while the server is still live. So a new flake is possible. Confirmed. | Have the check return the matching pair, and assert on that pair, so the assert uses one reading. | low |
| 13 | Landing folder | `crates/ferry-runtime/src/engine/api/transfers.rs:450-458`; rules at `crates/ferry-core/src/path.rs:104-131` and `crates/ferry-core/src/ops.rs:473` | No path escape is possible. A listed entry name that is empty or holds a `/` is refused on decode at `ops.rs:473`. `RemotePath::parse` refuses `..`, `.`, a backslash, a leading `/`, a NUL byte, and over 1024 bytes. So a peer root named `..` makes `mkdir` fail, not escape. Two smaller gaps remain. `parse` accepts any other control character, and a peer's listed name is not held to the 1 to 64 byte rule `roots.rs:356` applies to this device's own roots. So a modified peer can make `landing_folder` return a string holding a newline or a tab, and `DeviceDetail.swift:38` shows it. `landing_folder` also returns the raw `format!` string, not the parsed and normalised one, so a name with a doubled slash returns a string that differs from the folder `mkdir` made. Confirmed. | Return `partial.as_str()` from the parsed `RemotePath`, and refuse a root name that breaks `validate_root_name`'s rules before joining it. | low |
| 14 | Phone lifecycle | `android/app/src/main/kotlin/app/ferry/engine/FerryEngineShare.kt:70`; `android/app/src/main/kotlin/app/ferry/MainActivity.kt:153` | `pushShared` catches `FerryException` only. `uniffi.ferry_runtime.InternalException` extends `kotlin.Exception`, not `FerryException`, at `ferry_runtime.kt:254` and `:4913`. A Rust panic inside `landingFolder` or `pushFiles` escapes that catch. It also escapes `MainActivity`'s catch, because `pushShared` returns as soon as it launches its own coroutine. So the panic kills the process. Confirmed. | Catch `Throwable` in the `pushShared` coroutine and map anything that is not a `FerryException` to an app-side code with words. | low |
| 15 | On-disk writes | `crates/ferry-runtime/src/privatefile.rs:57` | The rename is not followed by a sync of the directory that holds the file. A power loss right after the rename can leave the directory entry unwritten, so the file reverts to the old content or disappears. This is not a regression. None of the five old copies synced the directory either, checked with `git grep sync_all 2be6a97 -- crates/`, which found only the five file syncs and `peers.rs`. Confirmed. | Open the parent directory after the rename and call `sync_all` on it, inside `write_atomic_with`, so all five callers gain it at once. | low |
| 16 | CI | `.github/workflows/ci.yml:34-35` and `:44-45`; `scripts/gate.sh:44-62` | Nothing the old steps checked is unchecked now. See the "Found right" list below for the step by step match. One thing did change. Each step's output now goes to `target/gate.log`, and only the last 40 lines print to stderr on a failure (`gate.sh:59`). The old steps streamed the whole output into the Actions log. A failing `cargo test` in CI now shows at most 40 lines. Confirmed. | Print the whole step file on failure when `CI` is set, or upload `target/gate.log` as an artifact. | low |

## Found right

- The five on-disk stores are behaviour-identical after 3366e80. I compared
  each old body with `privatefile.rs`. The temporary name is
  `<path>.<session>.tmp` in all five, which is the same directory as the
  target. `sync_all` runs on the file before the rename in all five. A
  failed write removes the temporary file and returns
  `TransferError::Local` in all five. `SessionId::generate` failing returns
  `TransferError::NoRandomness` in all five. A crash between the steps
  leaves the old file whole, because the rename is one step.
- The private mode is kept where a caller set it. `held.rs:193`,
  `auto_copy.rs:132`, and `networks.rs:243` call `write_atomic_private`,
  which sets mode `0o600` in the same `open` syscall (`privatefile.rs:89-97`).
  `record.rs:306` and `batch.rs:174` call `write_atomic`, which keeps the
  process's create mode, as both did before.
- `held.rs` keeps its own append path with its own `0o600` create
  (`held.rs:356-362`). That path was not part of the dedupe and is
  unchanged.
- `ferry-core`'s `peers.rs` keeps its own copy of the pattern on purpose.
  The module note at `privatefile.rs:21-23` states why, and the core is not
  changed by this batch.
- The shared `write_all_remote` keeps the same chunk boundaries as both old
  copies. `cap` is `limits::MAX_WRITE_LEN` and `piece` is
  `(bytes.len() - written).min(cap)`, at `push.rs:782-785`.
- The `.ferry-part` suffix is the same string in the one shared
  `partial_path` at `push.rs:767`. Both old copies built it the same way.
- Error mapping on the wire path is preserved for both callers.
  `push.rs:607` maps a `PathError` with `from_path`, as its old copy did.
  `dav/put.rs:321` maps it to `RpcError::Remote(OpError::InvalidPath)`, as
  its old copy did.
- `dav/put.rs` keeps its own running byte count. `land_new` at `:319`
  passes its caller's `&mut u64` straight through, and the tests at `:504`
  and `:540` still assert on it.
- The shared function is stricter than `dav/put.rs`'s old copy, on purpose.
  The old copy accepted a peer that claimed more bytes written than it was
  sent. The shared one refuses that at `push.rs:800-801`, before it adds to
  the count. So a landing against an over-claiming peer now fails fast, and
  its logged count no longer includes the over-claim. The commit message
  states this.
- `push.rs`'s `send` passes a scratch counter it never reads, at `:611`
  and `:657`. It logged no running count before either.
- No engine callback can reach the Mac model before `finishStarting` sets it
  up. The listener is built at `EngineModel.swift:176` and handed to
  `Engine` at `:177`, inside `finishStarting` itself. The method is main
  actor isolated and holds no `await` between `:164` and `:195`. Every
  handler in `EngineEvents.swift:25`, `:32`, `:39`, and `:46` only enqueues
  a `Task { @MainActor in ... }`.
- No `@MainActor` isolation was dropped by the six-file split. The class
  keeps the attribute at `EngineModel.swift:41`, and an extension of a
  globally isolated type inherits it. The two new `nonisolated` members,
  at `EngineModel+Settings.swift:148` and `:154`, touch no instance state.
- No `@Published` property is written from a non-main context. Every
  `Task.detached` that touches model state hops back with
  `await self?.method(...)`.
- `updateRevealPaths` cannot report a wrong path. The stale key it may
  write is never read, because the consumer at `EngineAdapter.swift:78`
  looks up only ids in the current list.
- `ShareIntake.launchOnIo` reports a failure the same way the old bare
  thread did. Commit c8def97 changed only `Thread {` to
  `ShareIntake.launchOnIo {` at `MainActivity.kt:145`. The try body, the
  `catch (t: Throwable)`, the `Log.w`, and the `setAppError` call are
  unchanged.
- `FerryEngineShare.kt`'s extension functions lost no guard. The engine
  null check is still `required()` at `FerryEngineDocuments.kt:63-64`. The
  `appContext` null check is still at `FerryEngineShare.kt:57` and `:69`.
  The empty devices check is still at `:39-41`. No old member function was
  `@Synchronized`, so no lock moved.
- The `started` flag order is still right after the split. `_batches` is
  written at `FerryEngine.kt:287` and `_started` at `:299`. The sweep waits
  on `started` at `ShareIntake.kt:129`, so it always sees a loaded list.
- Every reader of `accessLogRetentionDays` handles null. There is one
  reader in app code, at `AccessLogScreen.kt:62`, guarded by `?.let` at
  `:124`. The footer is simply absent before the first read. The old
  hardcoded 30 is gone from `res/`.
- The `landingFolder` errors all reach the person through the generated
  words table. On the Mac, `EngineModel+Sending.swift:91` calls `report`,
  which builds a `ThreePartError` at `EngineModel.swift:233`, and
  `DeviceDetail.swift:143` does the same. On the phone,
  `FerryEngineShare.kt:70-72` sets `_error`, which `FerryApp.kt:174-179`
  turns into words ahead of any share fault.
  `RootsError::NoRoots`, `OpError::PermissionDenied`, and
  `Runtime::NotReachable` all have words in `Errors.swift` and `Errors.kt`.
  Only the `NoRoots` wording is wrong for this case, which is finding 3.
- The peer's `kind` is read from this device's own stored peer list, at
  `transfers.rs:432-436`, not from the listing. A peer that lies about its
  kind at pairing gets `Ferry` instead of `Download` on its own disk, which
  costs nothing here.
- The poll helper consolidation did not change when `describe()` runs.
  `assert!(cond, "fmt", args)` expands to a `panic!` inside an `if`, so
  `describe()` is still called only on failure (`paths.rs:198-202`).
- `item_17_prefetch.rs` still keeps `wait_until_flag` at `:562`. That is a
  different helper, for an `AtomicBool`, not a second copy of the poll loop.
- `resume_sweep.rs` keeps its own `PATIENCE` and `POLL_TICK` and only wraps
  `poll_every` (`resume_sweep.rs:347-351`). Its budget is unchanged.
- The 60 second backoff floor in the batch restart test blocks no later
  assertion. `batch_paths.rs:193` sets it on the first engine only. The
  engine built after the restart, at `:250`, comes from `make_engine` and
  has the default floor. Every assertion after the restart, at `:265` and
  `:280`, reads `transfers()` and `batches()` once and does not wait for the
  resumed transfer. The peer engine is stopped at `:244`, so nothing can
  finish.
- `gate.sh rust` runs the same three commands the old steps ran:
  `cargo fmt --all --check`, `cargo clippy --all-targets --all-features --
  -D warnings`, and `cargo test --all --all-features` (`gate.sh:116-118`).
- `gate.sh gen` runs the same three checks: `gen_tokens.py --check`,
  `gen_errors.py --check`, and the bindings diff (`gate.sh:131-135`). The
  bindings helper at `:66-89` runs `gen_bindings.sh`, diffs the same three
  Mac files, and runs the same `diff -r` on the Kotlin uniffi folder. It
  writes into `target/` instead of `/tmp`, so two worktrees do not share it.
- The two steps the gate does not cover are still in `ci.yml`. The plain
  `cargo test --all` is at `:38-39`, and `cargo audit --deny warnings` is
  at `:60-61`.
- The cargo cache step is unchanged. The paths and the key at `ci.yml:25-30`
  are the same text as at 2be6a97, so it still keys on `**/Cargo.lock` and
  `rust-toolchain.toml`.
- `scripts/gate.sh` is committed with mode 100755, so CI can run it
  directly.

## Fix pass, 17 September 2026

All 16 rows are fixed and merged on main, one commit per row. Mac rows 1,
2, 4, 9, and 10 are commits 49517ad to a457ffd on `fix-mac`. Phone rows 5,
6, 8, and 14 are a5ee66d to be96d9f on `fix-phone`. Engine, test, and CI
rows 3, 7, 11, 12, 13, 15, and 16 are 840a895 to 359b469 on `fix-rust`.
Row 1's flag, row 2's caption, row 6's start snapshot, and row 7's output
files were read by the orchestrator against the Fix column before the
merge. Row 9 became a gate step, because Swift cannot limit a setter to
the other files of one module. Row 3 added the code
`Runtime::PeerSharesNothing`, whose words name the peer. Row 7 removed
adb's reader threads entirely: the child writes to files in a directory
made for that one call.
