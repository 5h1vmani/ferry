# The UX gestures audit

Date: 16 September 2026
Commit: 66d12c2, the merge of `ux-mac` and `ux-phone` into main. The range
read is `9017ff9..66d12c2`.
Scope: on the phone, the share target and the cache copy. On the Mac, the
drop targets, the Finder service, and the Send files panel. On both, the
transfer notifications, the Dock badge, and the menu bar lines. The promises
are `docs/ux-fix-plan.md` items 1 to 5. They are also `docs/engine-contract.md`
item 5, "Where a push lands".
Method: read only. No build, no test, no app run. Every listed file was read
in full at 66d12c2, plus `design/errors.json` and `crates/ferry-runtime/src/errors.rs`
for the error codes. "Confirmed" means the whole path was read in the repo.
"Plausible" names the step that was not read.

## Findings

| No. | Platform | Where (file:line) | What happens | Fix | Severity |
|---|---|---|---|---|---|
| 1 | Phone | `android/app/src/main/kotlin/app/ferry/ShareIntake.kt:151-158` and `:185-202` | The MediaStore branch queries `_data` with Ferry's own identity. Ferry holds `MANAGE_EXTERNAL_STORAGE` (`AndroidManifest.xml:16`). The code never checks that the sharing app had a grant on that URI. So an app with no storage permission can name any MediaStore row, such as `content://media/external/images/media/42`. Ferry reads that file and pushes it to the Mac. The person sees no file name and no confirm step before the push starts. Confirmed in code. The platform step not read is whether MediaStore serves the query, which `MANAGE_EXTERNAL_STORAGE` makes near certain. | Drop the `_data` branch, or gate it. Call `context.checkUriPermission` for the calling package first, or open the URI with `openInputStream` and use that stream as the only source. The contract already says the phone accepts only URIs "from the app that shared them". | high |
| 2 | Phone | `ShareIntake.kt:159-164`, `:207-227`, `:242-250`; `MainActivity.kt:136-144` | `ContentResolver.query` and `openInputStream` throw `SecurityException` when the sender sets no grant flag, and `IllegalArgumentException` for a malformed URI. `resolveOne` catches only `IOException`. `handleSharedUris` runs the work on a bare `Thread` with no handler. Any app on the phone can kill the Ferry process with one `ACTION_SEND` intent. That ends the foreground service and every running transfer. Confirmed. | Catch `Throwable` around each resolver call in `ShareIntake`, and treat a failure as `Resolution.Unreadable`. Wrap the body of the thread in `MainActivity` in a try block too. | medium |
| 3 | Phone | `ShareIntake.kt:225` | `source.copyTo(output)` has no size cap, no free space check, and no timeout. A hostile provider can serve an endless stream and fill the phone's storage. A provider that never answers leaks one thread per share. `ACTION_SEND_MULTIPLE` also has no cap on how many URIs it copies. Confirmed. | Cap one copy at a stated byte limit, and cap the number of URIs one share may carry. Stop and delete the partial file when the cap is passed. Put both constants in the contract. | medium |
| 4 | Mac | `macos/Ferry/Engine/FerryServicesProvider.swift:26`; `macos/Ferry/Engine/EngineModel.swift:693-699` | `readObjects(forClasses:options:)` passes no options, so it reads every URL on the pasteboard, not only file URLs. `send` then calls `urls.map(\.path)` with no `isFileURL` check. A URL such as `https://host/Users/name/.ssh/id_rsa` becomes the local path `/Users/name/.ssh/id_rsa`, and that real file is pushed. The same gap is in the two drop targets, which use `dropDestination(for: URL.self)`. Confirmed in code. Not read: whether AppKit and SwiftUI actually hand a web URL to these two entry points. | Pass `options: [.urlReadingFileURLsOnly: true]` in the service. Add `guard url.isFileURL` inside `send`, and refuse the whole call with a three part error when any URL fails it. | medium |
| 5 | Mac | `EngineModel.swift:263-272`, called from `:239` and from `start()` at `:161` | `notifyEndedTransfers` reads `lastGroupStates[group.id]` and treats a missing entry as a change. `start()` calls `reloadTransfers` with the whole stored history. So every stored Done or Failed group posts a notification at launch. `requestNotificationAuthorizationIfNeeded` at `:253-258` fires at the same moment, so the permission prompt appears at launch, not when a transfer starts as item 2 promises. Confirmed. | Seed `lastGroupStates` from the first read without notifying. Set a flag after the first `reloadTransfers` and notify only from the second one on. | medium |
| 6 | Phone | `android/app/src/main/kotlin/app/ferry/engine/FerryEngine.kt:561` | `pushShared` sends to `_devices.value.firstOrNull()`. It ignores which device is reachable and which the person is looking at. With two Macs paired the files go to whichever the engine lists first, with no way to choose. The Mac has a stated rule for this in `EngineModel.targetDevice` at `:337`. The phone has none. Confirmed. | Use the same rule as the Mac: the only paired device, else the first reachable one. Show the device step when two are reachable. | medium |
| 7 | Phone | `ShareIntake.kt:260-268` and `:273-285`; `FerryEngine.kt:561`, `:580` | A cache copy is written before the push and registered only after `pushFiles` returns a batch id. When the push throws, or when no device is paired and `pushShared` returns at `:561`, no registry entry is written. `sweepOrphaned` walks only the registry folder, never the cache folder. Those copies stay on disk for the life of the install. Confirmed. | Make `sweepOrphaned` list the cache folder itself and delete every folder no registry entry names. Register the paths before the push, not after. | medium |
| 8 | Phone | `FerryEngine.kt:596` | `landingFolder` throws `Runtime::NoCandidate` when the Mac lists no root. That code is a pairing code. `design/errors.json` gives it the words "Nothing was picked. That candidate is no longer present. Pick another." Those words say nothing true about a share. The Mac got this right with `DropError.noLandingFolder` and its own words. Confirmed. | Add an app side error on the phone, in the shape of `DropError` on the Mac, with its three parts in `strings.xml`. | medium |
| 9 | Phone | `android/app/src/main/kotlin/app/ferry/ReachableService.kt:88-91` | The retry branch returns before `startForeground` and returns `START_NOT_STICKY`. Android uses the last returned value, so one Retry tap turns off the sticky restart for the reachable service. When the process has died, the same tap builds a service whose engine has been created but not started, so `FerryEngine.retry` fails silently. Confirmed. | Return `START_STICKY` from the retry branch. Call `FerryEngine.start()` before the retry, and post the failure when it does not start. | low |
| 10 | Phone | `android/app/src/main/kotlin/app/ferry/FerryApp.kt:157-162`; `ShareIntake.kt:126` | The share error is tested before the engine error, so it hides a live engine error. Nothing clears it. `_unreadableName` is reset only at the start of the next `resolve` call. The error block stays on Devices until another share happens. Confirmed. | Clear `_unreadableName` when the person leaves Devices, or give the block a control that clears it, as `clearError` does for the engine error. | low |
| 11 | Mac | `macos/Ferry/Components/DeviceRow.swift:51-55`; `macos/Ferry/Screens/DeviceDetail.swift:92-96`; `macos/Ferry/FerryApp.swift:92-95` | A drop on a device that is not reachable returns false and shows nothing. A Dock drop with no target device returns and shows nothing. `docs/voice.md` rule 10 says nothing is hidden behind a friendly word, and a silent refusal hides more than a word does. Confirmed. | Add two `DropError` cases, one for not reachable and one for no device, and set `actionError` in each branch. | low |
| 12 | Mac | `EngineModel.swift:639-652` | `pushFiles` has no caller left. Its only caller was the Files section, removed in item 4. Confirmed by a repo wide grep at 66d12c2. | Delete the method. | low |
| 13 | Mac | `macos/Ferry/Strings.swift:142`; `EngineModel.swift:676` | `S.drop.downloadFolderName` holds the folder name "Download". `Strings.swift` is the file of words a person reads. This value is a path segment the contract fixes, and the phone must find the same folder. A translation of `Strings.swift` would change where files land. Confirmed. | Move the constant next to `landingFolderPath` in `EngineModel`, or into the generated contract constants, beside `LANDING_SUBFOLDER` on the phone. | low |
| 14 | Mac | `DeviceDetail.swift:136-142` | `revealPath` calls `FileManager.fileExists` while the view body is built. It runs on the main actor, once per transfer row, on every redraw. `reloadTransfers` fires up to four times a second. Confirmed. | Compute the reveal path when a group reaches Done, store it on the snapshot, and read it in the body. | low |
| 15 | Mac | `macos/Ferry/Engine/TransferNotifier.swift:46-47` | A Failed group whose `error` is nil returns a nil body, so `notify` posts nothing. Item 2 promises one notification when a transfer ends Done or Failed. The phone covers the same case at `ReachableService.kt:326-330`. Confirmed. | Fall back to `S.common.unknownErrorStopped`, the same words `ThreePartError` uses for an unknown code. | low |
| 16 | Phone | `ReachableService.kt:75-83`, `:140-141`, `:253-277` | A running transfer's notification is ongoing. `lastNotified` lives only in the service instance. When the process dies during a transfer, the progress notification stays in the shade with no one left to cancel it. `onCreate` does not clear the transfers channel. Confirmed. | Cancel every notification in the transfers channel in `onCreate`, before the collector starts. | low |
| 17 | Both | `FerryEngine.kt:575`; `EngineModel.swift:707` | The string `"OpError::AlreadyExists"` is typed by hand in Kotlin and again in Swift. It is a real code, from `crates/ferry-runtime/src/errors.rs:116`, and it is in `design/errors.json`. Neither app reads it from generated code, so a rename in Rust breaks both silently. Confirmed. | Generate a code constant beside the words table, and compare against that. | low |

## Found right

- A `file://` URI in `EXTRA_STREAM` is refused. `copyToCache` throws on any
  scheme that is not `content`, at `ShareIntake.kt:208-210`. Item 5's rule
  "content URIs only" holds.
- A copied file is named by the last path segment alone, at
  `ShareIntake.kt:232-240`. An empty name, ".", "..", a slash, and any
  control character all become a fresh UUID. NUL is covered by
  `isISOControl`.
- Each copy gets its own UUID folder, at `ShareIntake.kt:215`. The canonical
  parent check at `:220-222` refuses anything that would land outside it.
  The two checks together cover the "../../shared_prefs/x.xml" case the
  comment names.
- A `_data` path outside external storage is not handed to the engine. The
  canonical check at `ShareIntake.kt:172-180` resolves symlinks first, so a
  link out of the shared root falls through to a cache copy.
- The landing folder matches the contract on both sides. The phone builds
  the Mac's folder as the root named "Downloads", ignoring case, else the
  first root, plus "Ferry", at `FerryEngine.kt:592-598`. The Mac builds the
  phone's folder as the first root plus "Download", at
  `EngineModel.swift:672-677`.
- `mkdir` runs before the push on both sides, and `OpError::AlreadyExists`
  is treated as success. `FerryEngine.kt:569-578` and
  `EngineModel.swift:704-713`.
- `PermissionDenied` is surfaced. The phone sets `_error` at
  `FerryEngine.kt:582`. The Mac calls `report`, which sets `actionError`, at
  `EngineModel.swift:714-716` and `:815-816`. Both flow into `ErrorBlock`.
- An unreachable push fails at once and is shown. `push_files` dials first,
  per item 5, and the resulting `Runtime::NotReachable` reaches the same two
  error paths.
- The mount prefix test does not confuse "/Volumes/Pixel" with
  "/Volumes/Pixel 2/x". `mountedLocation` appends a separator before the
  prefix test, at `EngineModel.swift:750-758`.
- A remote path built from a URL with ".." segments is refused by the
  engine. `RemotePath` rejects "..", per `crates/ferry-core/src/path.rs`,
  which the 11 September audit read. The result is a clear failure, not an
  escape.
- A dropped folder is refused as a whole and never pushed file by file.
  `send` tests every URL with `allSatisfy` before it reaches the engine, at
  `EngineModel.swift:694-697`. `isFolder` follows symlinks, so a link to a
  folder is refused too.
- `EngineModel` is `@MainActor`, at `EngineModel.swift:33`. So the Dock
  badge, the notifications, and `actionError` are all set on the main
  thread. The Finder service hops to the main actor before it touches the
  model, at `FerryServicesProvider.swift:32`.
- Notification authorization is asked once per run, guarded by
  `didRequestNotificationAuthorization` at `EngineModel.swift:84` and `:253-258`.
- The Mac posts one notification per end state, not one per 250 ms tick.
  `notifyEndedTransfers` compares against `lastGroupStates` at
  `EngineModel.swift:266-270`. The phone does the same with `lastNotified`
  at `ReachableService.kt:262-270`.
- `ReachableService` cancels its collector in `onDestroy`, at
  `ReachableService.kt:141`. The job is the only child of `notifyScope`, so
  no collector is left running.
- The service calls `startForeground` on both paths that keep it alive. They
  are the advertising path at `ReachableService.kt:130-134` and the engine
  failure path at `:119-124`. So the five second rule is met after a
  `startForegroundService` start.
- Disk work stays off the main thread where it matters. `ShareIntake.resolve`
  runs on a thread from `MainActivity:136`. The sweep and the reconcile run
  on `Dispatchers.IO` from `ShareIntake.kt:41`. `FerryEngine.stop` gets its
  own thread in both `onDestroy` methods.
- No string is written inline in a changed view. A grep for quoted English
  in `DeviceRow.swift`, `TransferRow.swift`, `MenuBarPresence.swift`,
  `DeviceDetail.swift`, `FerryApp.swift` and `DevicesScreen.kt` found only
  comments.
- No hex value, no point size, and no raw `dp` in a changed view.
  `RunningBatchLine` uses `FerryFont`, `FerryColor` and `FerrySpace` only.
- Error words are read from the generated table, not composed. The phone
  uses `FerryErrors.wordsFor` at `ReachableService.kt:411-422`. The Mac uses
  `FerryErrors.words(for:)` at `ThreePartError.swift:31`.
- Numbers go through `Format`. `ReachableService` uses the new
  `formatSize(Context, Long)` and `formatDuration(Context, Long)`, which
  read the same `strings.xml` templates as the Compose versions. The Dock
  badge goes through `FerryFormat.badgeCount`.
- The documents provider is untouched by this range, so the
  `MANAGE_DOCUMENTS` gate the 11 September audit read still stands.

## Not read

- The generated files `macos/Ferry/Generated/Errors.swift` and the phone's
  `FerryErrors`, beyond their call shape. The code list was read from
  `design/errors.json` instead.
- `crates/ferry-runtime/src/push.rs`. Item 5's own text was taken as the
  statement of what push does.
- `macos/Ferry/Screens/AccessLogSection.swift` and `docs/ia.md`, which
  changed in this range but sit outside the gesture paths.
- Whether AppKit or SwiftUI can deliver a non file URL to the service or to
  `dropDestination`. Finding 4 rests on the missing check, not on that step.

## Fix pass, 16 September 2026

All 17 rows are fixed, one commit each. Rows 1, 2, 3, 6, 7, 8, 9, 10, and
16 are on branch `ux-phone`, commits 0176a3f to 8f27c36. Rows 4, 5, 11,
12, 13, 14, and 15 are on branch `ux-mac`, commits fab1bd4 to 86f1455.
Row 17 took a generator change: `scripts/gen_errors.py` now emits
`FerryErrorCode` in both generated error files, one constant per row of
`design/errors.json`, and both apps compare against it, commits 5e765c8,
45c73f3, and the phone's twin. Rows 1, 3, and 6 added their rules to
`docs/engine-contract.md`, item 5, "Where a push lands": a share needs the
system's read grant, a share is capped at 100 files, 4 GiB per copy, and
512 MiB free, and the target device is the only paired one or the first
reachable one. Both branches are merged on main and the full gate ran
once on the merged tree.

## Narrow verification, 16 September 2026

The orchestrator read the fix hunks for rows 1 to 7, 9, and 16 on the
phone and rows 4 and 5 on the Mac against the Fix column. Rows 1 to 6, 16,
4, and 5 hold as written. Two did not:

- Row 7. `FerryEngine.start()` set `started` before it loaded the batch
  list. `ShareIntake` sweeps the cache on that signal and keeps only what
  the batch list names, so an empty list would delete a paused push's
  source file. The flag now turns true after the loads, commit cc27cee.
- Row 9. A Retry tapped from the shade returned silently when the engine
  could not start. It now posts the engine's error words on the same
  notification, commit 043f8c4.

Row 17 was wider than the audit said. A grep for a quoted `X::Y` code
found twenty-two sites across both apps, not two. All now read the
generated `FerryErrorCode` constant, commit 9d522ec. The one code with no
table row, `Android::KeyRenameFailed`, stays a literal.
