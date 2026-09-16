# Principles review, 16 September 2026

Commit: 513bf60. Scope: single source of truth, don't repeat yourself,
single responsibility, dead code, silent failure, stale comments, and
threading, across the Mac app, the phone app, the engine, the scripts, and
the tests. Method: three read-only Sonnet passes, one per area, with grep
confirmation of every dead-code row. Fixes are recorded at the end.

The rows below use the reviewers' own words, lightly cut. "Held" lists
what was checked and found fine.

## Mac app

### Single source of truth

| No. | Where | What | Smallest fix |
|---|---|---|---|
| M1 | `Engine/EngineModel.swift:704` and `FerryEngine.kt:240` | Both apps type the landing subfolder `"Download"`. | The engine owns the landing rule; see the engine section, E-new. |
| M2 | `Components/PresenceControl.swift:77-79` and `Screens/SettingsView.swift:101-103` | Both compute `networkName == nil ? quietUnknownNetwork : quietKnownNetwork`. | One computed property on `PresenceSnapshot`; delete both copies. |
| M3 | `Screens/PairingQRView.swift:60` | `.background(Color.white)` types a raw colour. | A comment stating why a QR needs true white, not a theme colour, as the phone's scanner overlay does. |

### Don't repeat yourself

| No. | Where | What | Smallest fix |
|---|---|---|---|
| M4 | `Components/TransferRow.swift:160-170` and `Engine/TransferNotifier.swift:41-49` | Both build "N files · bytes · duration" from the same fields. | One `doneSummary` on `TransferGroupSnapshot`; both read it. |
| M5 | `Components/DeviceRow.swift:74-79` and `Screens/PairingQRView.swift:107-112` | Both switch on `DeviceKind` to pick an icon. | Move the mapping onto `DeviceKind` in `Model/EngineTypes.swift`. |

### Single responsibility

| No. | Where | What | Smallest fix |
|---|---|---|---|
| M6 | `Engine/EngineModel.swift`, 979 lines | Seven concerns: lifecycle, devices and presence, transfers and notifications, pairing, mount, sending, settings. | Keep the class, stored properties, `init`, `start`, `stop`, `report` in `EngineModel.swift`. Move the rest into `EngineModel+Devices.swift`, `+Transfers.swift`, `+Pairing.swift`, `+Mounting.swift`, `+Sending.swift`, `+Settings.swift`, as `extension EngineModel`. Private members that cross files become `fileprivate` or internal. |

### Dead code

| No. | Where | What |
|---|---|---|
| M7 | `Strings.swift:391` `S.pairing.showANewCode` | No caller. |
| M8 | `Strings.swift:423-426` `stopsInFormat`, `stopsIn` | No caller on the Mac. |
| M9 | `Strings.swift:431` `S.deviceDetail.infoSection` | No caller. |
| M10 | `Components/TransportBadge.swift:12` `.connecting` | Never built outside a preview. |
| M11 | `Model/Snapshot.swift:36` `PresenceSnapshot.isReportedByEngine` | Set, never read. |
| M12 | `Model/Snapshot.swift:79` `DeviceSnapshot.speedBytesPerSec` | Set once, never read. |
| M13 | `Model/Snapshot.swift:93` `MountSnapshot.isReady` | No caller. |
| M14 | `Model/EngineTypes.swift:20-23` `Entry: Identifiable` and `EngineModel.swift:644-651` `list` | No caller for `list`; `Entry` is never listed. |

### Silent failure

| No. | Where | What | Smallest fix |
|---|---|---|---|
| M15 | `EngineModel.swift:240` `clearEjectedMounts` | `try?` drops a failure after a person ejects a volume. | Route through `report(_:)`. |
| M16 | `EngineModel.swift:543` `unmountAndStopBridge` | Same, on "Forget this phone". | Same. |
| M17 | `Screens/DeviceDetail.swift:140` | `try? await model.landingFolder` hides the reason a caption vanished. | Set `actionError` in a `catch`. |
| M18 | `Engine/NetworkName.swift:45,49` | `try?` drops a failure to start or stop Wi-Fi name monitoring. | Report it through presence, since it decides the "quiet on this network" line. |

### Stale comments

| No. | Where | Current fact |
|---|---|---|
| M19 | `EngineModel.swift:12-13` | `DeviceDetail` no longer browses `Entry`. |
| M20 | `EngineAdapter.swift:5` | This file never names `Entry`; `EngineModel.swift` does. |

### Threading

| No. | Where | What | Smallest fix |
|---|---|---|---|
| M21 | `FerryApp.swift:25-29` and `EngineModel.start()` | Directory creation and a Keychain read run on the main actor at launch. | Detach the disk and Keychain steps, hop back to publish. |
| M22 | `EngineModel.swift:238` | `fileExists` on the main actor per device per `devicesChanged`. | Move off the main actor. |
| M23 | `EngineModel.swift:269` | Same, per `reloadTransfers`. | Same. |

Held: every `Task.detached` reports through `report(_:)` on the main actor; `FinderMount.swift:79`'s swallowed unmount error is explained; no size literals outside `Tokens.swift`; every enum case has a caller; every cited path exists.

## Phone app

### Single source of truth

| No. | Where | What | Smallest fix |
|---|---|---|---|
| P1 | `screens/FirstRunScreen.kt:134`, `screens/PairingScreen.kt:530` | Both define `MIN_TARGET = 48.dp`; `FerrySpace.s7` is 48. | Delete both; use `FerrySpace.s7`. |
| P2 | `components/TransferRow.kt:120,139` | `4.dp` literal; `FerrySpace.s1` is 4. | Use `FerrySpace.s1`. |
| P3 | `engine/FerryEngine.kt:81` and `strings.xml:317` | The root name "Internal storage" is typed in the engine config and again as a Settings string. | Settings reads the root names from `roots()`; the string goes. |
| P4 | `components/TransportBadge.kt:90` | `" · "` typed; every other site reads `R.string.dot_separator`. | Use the resource. |

### Don't repeat yourself

| No. | Where | What | Smallest fix |
|---|---|---|---|
| P5 | `ErrorWords.kt:17-29` and `ReachableService.kt:430-441` | Same lookup, fallback, fill, and trim, once with `stringResource`, once with `getString`. | One plain function in `ErrorWords.kt` taking the resolved unknown-code strings; both become wrappers. |
| P6 | `Format.kt` `formatSize` and `formatDuration`, two overloads each | Identical arithmetic. | One private pure function per pair; each overload reads its resource. |
| P7 | `components/TransferRow.kt` four line builders and `ReachableService.kt` four twins | Same sentences built twice. | One plain function per pair taking a string-lookup function. |
| P8 | `ReachableService.kt:212-217, 305-310, 461-466` | The same five-line `PendingIntent.getActivity` block three times. | `private fun openAppIntent()`. |

### Single responsibility

| No. | Where | Smallest split |
|---|---|---|
| P9 | `engine/FerryEngine.kt`, 760 lines | Provider passthrough (lines 690-759) to `engine/FerryEngineDocuments.kt`; share handling (`pushFiles`, `pushShared`, `targetDevice`, `landingFolder`) to `engine/FerryEngineShare.kt`; key load to `engine/DeviceKeyStore.kt`. |
| P10 | `ReachableService.kt`, 524 lines | Transfer notification building and updating (lines 260-424) to `TransferNotifier.kt`. |
| P11 | `ShareIntake.kt`, 449 lines | Registry and sweep (lines 359-448) to `ShareCacheRegistry.kt`. |
| P12 | `provider/FerryDocumentsProvider.kt`, 522 lines | Id and path helpers and the errno map (lines 437-522) to `provider/DocumentIds.kt`. |
| P13 | `screens/PairingScreen.kt`, 536 lines | Waiting, code, and confirmed content with the countdown (lines 419-508, 316) to `screens/PairingWaitingContent.kt`. |

### Dead code

| No. | Where | What |
|---|---|---|
| P14 | `model/UiModels.kt:56` `DeviceInfo.mountPath`, set at `Mapping.kt:92` | Never read. Its comment at line 55 cites a README that does not exist. |

`FerryIcon.folder`, `.forget`, `.transfer` are unused on the phone but come from the shared `design/tokens.json`, which the Mac reads. They stay.

### Silent failure

| No. | Where | What | Smallest fix |
|---|---|---|---|
| P15 | `engine/FerryEngine.kt:348, 374, 384, 399, 425, 455, 492, 507, 523` | `val current = engine ?: return` on paths a tap triggers. | Set `_error` to `FerryErrorCode.RUNTIME_NOT_STARTED` before returning. |
| P16 | `ReachableService.kt:478` | A Retry tap with a missing extra returns silently. | Fall back to `postRetryFailed` with the unknown-code words. |

### Stale comments

| No. | Where | Smallest fix |
|---|---|---|
| P17 | `strings.xml`, 16 comments citing `docs-v2/` | `sed 's|docs-v2/|docs/|g'` on that file. |
| P18 | `model/UiModels.kt:55` | Goes with P14. |

### Threading

| No. | Where | What | Smallest fix |
|---|---|---|---|
| P19 | `MainActivity.kt:144` | A bare `Thread` for disk and network work. | Launch on `ShareIntake`'s IO scope. |

Held: the two bare threads that stop the engine are explained in place; no `runCatching`; every `catch` sets an error or rethrows for the platform; the provider's `HandlerThread` is the platform's contract; the scanner overlay's black and white are explained.

## Engine, scripts, and tests

### Single source of truth

| No. | Where | What | Smallest fix |
|---|---|---|---|
| E1 | `notify.rs:20` `HOLD` and `engine.rs:103` `ACCESS_LOG_TICK`, both 250 ms | Two constants, one value, a comment saying they must match. | `engine.rs` uses `notify::HOLD`. |
| E2 | `access.rs:122` `RETENTION_DAYS = 30` and "Kept for 30 days." typed in `Strings.swift:281` and `strings.xml:170` | The number lives in three places. | Export `access_log_retention_days()` over UniFFI. The apps format "Kept for %d days." from it. The app half lands after the bindings regenerate. |
| E3 | `limits.rs:35` `MAX_READ_LEN` and the prose "1 MiB" in `design/errors.json:366` and `docs/protocol.md:320` | Prose restates the cap. | Name the constant in the protocol table and in a doc comment on the constant that cites both prose sites. |
| E-new | `EngineModel.swift` `landingFolderPath` and `FerryEngine.kt` `landingFolder`, plus `targetDevice` in both | The landing rule, the `mkdir` first step, and the `AlreadyExists` check are written twice, in two languages. | One engine call, `landing_folder(device_key_hex) -> Result<String, FerryError>`, that lists the peer's roots, picks per the peer's `kind` as contract item 5 says, calls `mkdir`, treats `AlreadyExists` as success, and returns the root-relative folder. The constants move into the engine and the contract. Test file `item_20_landing_folder.rs`. The apps adopt it after the bindings regenerate. |

### Don't repeat yourself

| No. | Where | What | Smallest fix |
|---|---|---|---|
| E4 | `encode_i64`/`decode_i64` in `peers.rs:335`, `ops.rs:687`, `access.rs:355`, `held.rs:387`, `auto_copy.rs:221` | Five identical copies. | One `pub` pair in `ferry-core`; four callers. |
| E5 | `write_and_sync`, `open_new_private_file`, `temporary_name`, `write_private_file` in `held.rs`, `auto_copy.rs`, `networks.rs`, `record.rs`, `batch.rs` | Five copies of write-temp, sync, rename. | One module `ferry-runtime::privatefile`; five callers. |
| E6 | `to_hex` in `peers.rs:513` and `discovery.rs:255` | Identical. | One `pub(crate)`. |
| E7 | `unix_secs_to_system_time` in `localfs.rs:624` and `dav/probes.rs:211` | Identical, two crates. | `ferry-core`'s becomes `pub`. |
| E8 | `partial_path` and `write_all_remote` in `push.rs:754-788` and `dav/put.rs:429-465` | Near identical; `put.rs` says so. | One shared helper with the byte count as a parameter. |

### Single responsibility

| No. | Where | Smallest split |
|---|---|---|
| E9 | `access.rs`, 1683 lines | Codec helpers (lines 316-604) to `access/codec.rs`. |
| E10 | `tcp.rs`, 1506 lines | `DeadlineStream` (lines 301-357) to `tcp/deadline_stream.rs`. |
| E11 | `noise.rs`, 1282 lines | The IK functions and `IkAccepted` (lines 439-580) to `noise/ik.rs`. |
| E12 | `dav/handlers/write.rs:516-647` `put_file`, 132 lines | Content-length checks to `checked_content_length`. |
| E13 | `dav/handlers/browse.rs:140-268` `propfind`, 129 lines | The preamble to `propfind_self_entry`. |

### Stale comments

| No. | Where | Current fact |
|---|---|---|
| E14 | `lib.rs:65` "Per-peer connection caps are not built" | `MAX_SERVING_PER_PEER` is enforced at `engine/serving.rs:75`. |

### Scripts

| No. | Where | What | Smallest fix |
|---|---|---|---|
| E15 | `gate.sh` `gen_bindings_check` and `ci.yml` "Check the generated app bindings" | The same diff typed twice. | `ci.yml` runs `scripts/gate.sh gen`. |
| E16 | `gate.sh` `mode_rust` and `ci.yml` "core" | fmt, clippy, test typed twice. | `ci.yml` runs `scripts/gate.sh rust`, then its own no-features test and `cargo audit`. |
| E17 | `gen_tokens.py:318-366` and `gen_errors.py:209-256` | `load_json`, `read_existing`, `write_file`, and the check-or-write flow. | `scripts/gen_common.py`, imported by both. |

### Tests

| No. | Where | What | Smallest fix |
|---|---|---|---|
| E18 | `common/engines.rs:96` `wait_until`, `item_17_prefetch.rs:53`, `item_6_spool_count.rs:51`, `resume_sweep.rs:341` `poll_until`, and `common/paths.rs` `poll_until` and `poll_until_or_describe` | Six versions of poll, sleep, panic past a deadline. | One free function in `tests/common/mod.rs`, with the describe variant, called by all. |

Held: workers, backoff floor, and chunk size are single-sourced; the two DAV status maps differ on purpose; no dead `pub(crate)` item among 327; `rpc.rs`'s "not built" notes are true; no test file over 1000 lines.

## Fix pass, 16 September 2026

Every row is fixed and merged on main, one commit per row or per group of
like rows. Mac rows M2 to M23 are commits a41d4b9 to ca140a2 on branch
`tidy-mac`; `EngineModel.swift` went from 979 lines to 229 plus six
extension files. Phone rows P1 to P19 are commits ab0a84a to 5caf6a7 on
`tidy-phone`; `FerryEngine.kt` went from 760 lines to 615,
`ReachableService.kt` from 524 to 351, with eight new files by concern.
Engine rows E1 to E18 and E-new are commits dc36f3b to b85d899 on
`tidy-rust`; the engine gained `landing_folder` and
`access_log_retention_days`, and `ci.yml` now calls `scripts/gate.sh`.
Row M1 and the app halves of E2 and E-new landed after the bindings
regenerated: the Mac in edeb98b and 7443820, the phone in 91d02c3 and
ac64e55. Both apps deleted their landing constants, their `mkdir` step, and
their `AlreadyExists` check, and both format the retention line from the
engine's number.

Found during the pass, outside the review: the CI job had failed on every
run since 11 September in one test that assumed a 4 MiB pull finishes
within the one second backoff floor. Fixed in 513bf60. The hung adb test
flaked once under parallel load with a bare assertion; 273ed1c makes it
name the value it got. Reading it further found a real defect: the call
joined adb's output readers after a timeout, and the shell's `sleep` child
kept the pipes open, so every such call, and that test, lasted 30 seconds.
The call now returns at the deadline without the join, and two tests
assert it returns within five seconds.
