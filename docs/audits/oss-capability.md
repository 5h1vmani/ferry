# Audit: capability and security before the public release

Date: 25 September 2026.
Commit: `d678d59`, on `main`.
Scope: every factual claim in `README.md`, real-device status, and the attack surface a public release exposes.
Method: targeted reads and greps. One test was run: the ignored full resume sweep, to settle its point count.
Labels: "confirmed" means the code path was read end to end. "Suspected" names the step that would prove it.

## Verdict

The engine's claims hold in code and in tests, and no unpaired stranger can read or write a file.
The README overclaims in the two places a reviewer checks first, the lead test number and the Status paragraph.
Nothing a first user touches has run on a real device, and three Android and Mac platform gaps will likely fail at first use.

## Claims table

| Claim in README.md | Verdict | Evidence |
|---|---|---|
| Every cut in a transfer resumes and refetches at most one chunk (line 5). | Partly holds | The bound holds for pulls, one cut per transfer, at `resume_sweep.rs:424-503`. `set_cut` arms one dial only (`engine/api/lifecycle.rs:606-611`). Pushes are cut at three points only (`push_paths.rs:191-195`). A Finder copy does not resume at all (`dav/put.rs:22-25`). A copy in the phone's Files app is a plain write (`FerryDocumentsProvider.kt:300`). |
| A test cuts the wire after every byte, 4718 cut points (lines 5-7). | Does not hold as written | The ignored test ran today and passed with 4962 points over 4963 clean bytes, in 357 s. The number predates the manifest request and the kind byte in `hello`. The test is `#[ignore]` (`resume_sweep.rs:482`), and neither `scripts/gate.sh` nor `.github/workflows/ci.yml` passes `--ignored`. The file is 4396 bytes at 1 KiB chunks against an in-process fake peer (`resume_sweep.rs:122-125`). |
| Phase 1 has run on a Mac and a Pixel 3 XL; pairing, a file copied, and the cable work (lines 9-10). | Partly holds | The run was on 10 September (`manual-checks.md:105-110`, commit a5da751). The copy used the Files section, which was removed on 16 September (`ux-fix-plan.md:146`). Commit b12ac19 rewrote task 3 Part D on 17 September to Finder and Services steps that never ran. The Mac and phone screens used in that run were both replaced on 11 September. |
| Named shared folders, folder copies as one batch, an access log on both sides (lines 11-12). | Holds in code | `roots.rs`, `folder.rs:107-165`, `access.rs`. None ran on a device (task 4 is open). |
| A manifest request so every pull verifies from its first byte (lines 14-15). | Holds | The sweep predicts the manifest frame to the byte (`resume_sweep.rs:173-202`), and the clean pull matches it exactly. |
| Automatic photo import and push (line 15). | Holds in code | `auto_copy.rs`, `push.rs`. No manual check covers automatic import at all. |
| The Finder mount over WebDAV (line 16). | Partly holds | Tests drive the bridge with a Rust HTTP client (`tests/dav_*.rs`). The only real macOS WebDAV client run mounted a throwaway probe, not this bridge (`spike-0-findings.md:7-24`). |
| Saves that send only changed chunks (lines 16-17). | Partly holds | `dav_write.rs:145-182` proves it for a second `PUT` to the same path with one byte changed. Chunks are at fixed offsets, so an insert near the start resends the whole file. Suspected: app saves write a new temporary name and then `MOVE`, which lands as a full new file. See finding M7. |
| QR pairing on the Mac, and the phone scanning it (lines 17-20). | Holds in code | `engine/pairing.rs:700-746`, `tests/two_engines_qr_pairing.rs`, `tests/item_12_scan_confirm.rs`. Never run on a device. |
| Each was audited and the findings fixed (line 17). | Partly holds | Third-run finding 9 is not fixed, by decision (`third-run-engine.md`, "Not fixed"). Security finding 2 was fixed without its "log full" entry, and finding 6 is recorded as a limit (`fable-security.md:48-60`). |
| A lifecycle audit found fifteen findings, every one fixed (lines 19-20). | Holds | `kotlin-lifecycle.md:26` and `:43`. |
| A head cache and prefetch, so a thumbnail costs nothing on the wire (lines 21-22). | Partly holds | A 64 KiB head request is served from cache (`dav/heads.rs:34-40`, `tests/item_17_prefetch.rs`). The prefetch spends up to 512 x 64 KiB per folder in advance. Nobody measured whether Finder's thumbnailer stays inside 64 KiB. |
| Advertising and browsing limited to trusted Wi-Fi networks, on both apps (lines 22-23). | Does not hold in practice | The rule fails open on an empty list (`networks.rs:73-75`). The phone's location request is likely ignored by Android, and the Mac likely cannot read the network name. See finding M1. |
| The Mac's shared folders inside the phone's Files app (lines 23-24). | Holds in code | `FerryDocumentsProvider.kt`. It has no progress, retry, or resume (`FerryDocumentsProvider.kt:300`). |
| None of it has run on real devices; tasks 4, 5, and 6 are the list (lines 26-27). | Partly holds | Task 7 is missing from the list (`manual-checks.md:319-322`). The 16 September work in `ux-fix-plan.md` is not mentioned at all. |
| Ferry uses whichever path works, and shows which one is active. | Holds in code | USB is tried first (`engine/loops.rs:138-150`). |
| USB lands in the same throughput range as 5 GHz Wi-Fi. | Not supported | Nothing was measured (`decisions/0004-usb-is-reliability.md:30-34`). The reasoning is about Open Accessory (`:11-13`), and Ferry uses adb (decision 9). |
| USB works on networks that block discovery, and when the router does not. | Holds | Task 3 Part E worked with the phone's Wi-Fi off (`manual-checks.md:91-97`, `:105`). It needs Developer options and USB debugging left on, and the README does not say so. |
| A new connection resumes the same transfer from the manifest. | Partly holds | It resumes after a socket error. A silent link loss waits up to 300 s first. See finding M2. |
| On resume the receiver re-hashes what it already holds. | Holds | Pull: `transfer.rs:708` and `:866`. Push asks the peer for its manifest instead (`push.rs:43-54`). |
| Both directions share one protocol, one chunking scheme, and one resume path. | Partly holds | The protocol and chunking are shared. Push has its own resume path (`push.rs:43-54`). |
| Either device can serve the operations layer, and either can call it. | Holds | The Mac calls through `transfer.rs` and the bridge. The phone calls through `FerryDocumentsProvider.kt`. |
| Noise instead of TLS. | Holds | `noise.rs:39-53` uses XX, KK, and IK with ChaChaPoly. Commit and reveal is checked at `noise.rs:264-283` and `:422`. |
| No free tool lets a Mac user browse an Android phone in Finder; Android File Transfer ended in May 2024. | Not checked | These are market claims outside the code. |

## Never run on hardware

Only the 10 September build ran on devices. Every feature below has never run on a real Mac and phone pair.

- Resume after a real cut in the middle of a transfer, on either transport. Task 3 Part E copied a new photo over the cable; it did not cut a running transfer.
- The Mac's designed screens, the menu bar presence switch, and the Dock badge (task 4 Part A, task 7 Part C).
- Named shared folders served to the phone, and adding or removing one live (task 4 Part B).
- A folder copy as one batch, its resume on the other transport, and its survival across an app restart (task 4 Part C).
- The access log on both sides (task 4 Part D, task 4 Part F step 4).
- The Finder mount: mount, browse, Quick Look, eject and remount, save, rename, new folder, delete (task 4 Part E).
- The delta on save, and the head cache and prefetch for thumbnails.
- The phone's designed screens and its notification actions (task 4 Part F).
- Pairing by scanning the Mac's QR code, with a confirm on both sides (task 4 Part G).
- The Mac's folders in the phone's Files app, and saving into them from another app (task 5).
- Trusted networks on both apps, and both location permission flows (task 6).
- Push from the Mac to the phone, by drag, Dock drop, Send files, and Cmd+O (task 7 Part D).
- The phone's share target and its Send files picker (task 7 Part A).
- Transfer notifications on both platforms, with Retry (task 7 Parts B and C).
- The Finder service "Send with Ferry" and the context menus (task 7 Part E).
- Automatic photo import. No manual check lists it.
- The phone's foreground service over many hours, and on any Android version above 12.
- A build by anyone other than the owner, on any Mac other than one Apple silicon machine.

Commit bfa4980 on 11 September describes a symptom "the person saw". No task records a result after 10 September, so I do not know whether an informal run happened.

## Findings

### Blocker

**B1. The README's lead claim is wrong in its number and wider than its proof.**
Evidence: `README.md:5-7`; `resume_sweep.rs:482`; `dav/put.rs:22-25`; `FerryDocumentsProvider.kt:300`.
Scenario: a reviewer runs the named test with `--ignored` and sees 4962 points, not 4718. The reviewer then finds that a Finder copy and a Files-app copy do not resume at all, and that CI never runs the sweep.
Fix: state the real count, name it as a pull with one cut, say it is run on demand, and say that Finder and Files-app copies do not resume yet.

**B2. The Status paragraph presents a device run of a flow that no longer exists.**
Evidence: `README.md:9-10` and `:26-28`; `ux-fix-plan.md:146`; commit b12ac19; `manual-checks.md:319-322`.
Scenario: a reviewer follows task 3 to reproduce "a file copied". The steps now use Finder and Services, which never ran, and task 7 is not in the README's list of open work.
Fix: say that only the 10 September build ran on devices, that its copy screen was removed, and that tasks 4 to 7 are all open.

### Major

**M1. Trusted networks likely never engage on either device, and the rule then fails open.**
Evidence: `networks.rs:73-75` treats an empty list as present everywhere. The phone asks for `ACCESS_FINE_LOCATION` alone through `RequestPermission` (`MainActivity.kt:79-80`, `:248`), and the manifest declares no coarse permission (`AndroidManifest.xml:49`). Android documents that an app targeting API 31 or later that asks for fine location without coarse has the request ignored; this app targets 36 (`build.gradle.kts:15`). The Mac runs with the hardened runtime and an empty entitlements file (`project.yml:63`, `Ferry.entitlements`), with no `com.apple.security.personal-information.location` entitlement.
Scenario: the phone shows no location prompt at pairing, so the SSID stays unknown and the trusted list stays empty. The phone then advertises and accepts on every network, which is the state the feature exists to prevent. Task 6 steps 2 to 5 would fail. The Android part is confirmed in code against Android's documented rule. The Mac part is suspected; a run on macOS 14 or later that checks `CLLocationManager.authorizationStatus` would prove it.
Fix: request coarse and fine together and declare both, add the hardened-runtime location entitlement, and treat "no name readable" as quiet rather than present once a device has paired.

**M2. A silent link loss stalls a transfer for up to five minutes before resume starts.**
Evidence: the dialing side sets a 300 s read timeout (`tcp.rs:223`, `:370-371`, `:895`). `CHUNK_DEADLINE` is checked only between reads (`transfer.rs:87`, `:948-957`), so it cannot interrupt a blocked read. No TCP keepalive is set anywhere in `crates/`. The cut in every test is an immediate `ConnectionAborted` error (`guard.rs:321-322`).
Scenario: task 4 Part C step 3 turns the phone's Wi-Fi off during a pull. The phone sends no reset, so the Mac's read blocks until the 300 s timeout, even with the cable plugged in. The person sees a frozen bar and concludes resume does not work.
Fix: put a read timeout of a few seconds on transfer connections, or a keepalive probe, and move the transfer to a new dial when the device becomes reachable on another transport.

**M3. On Android 15 and later the service will be stopped after six hours a day, and the app does not handle it.**
Evidence: `AndroidManifest.xml:86-92` declares `dataSync` and its own comment names the cap; `targetSdk = 36` (`build.gradle.kts:15`); `ReachableService.kt` has no `onTimeout` override (grep finds none).
Scenario: a person leaves advertising on. After six hours Android calls `onTimeout`, the service does not stop, and Android raises an exception that ends the process. From then on the Mac cannot reach the phone that day. Confirmed in code; the Pixel 3 XL runs Android 12 and cannot show it.
Fix: handle `onTimeout` by stopping cleanly and telling the person, and choose a service type that matches an always-on presence, or run the service only while a transfer or pairing is live.

**M4. The phone never takes a multicast lock, so mDNS may not receive on many phones.**
Evidence: `AndroidManifest.xml:28` declares `CHANGE_WIFI_MULTICAST_STATE`; `PLAN.md:212` lists a `MulticastLock`; a grep for `MulticastLock` under `android/` finds nothing. `PLAN.md:319-324` records that the Pixel 3 XL advertised without the lock and defers it until "a phone is not found". Discovery runs in Rust through `mdns-sd` (`discovery.rs:50`).
Scenario: Android documents that the Wi-Fi stack drops multicast without the lock, and many phones do. Such a phone can send its own announcements but cannot answer queries or see the Mac. A public user with such a phone never sees it on the Mac over Wi-Fi. Suspected; a run on a second phone model, or with the screen off after a Mac relaunch, would prove it.
Fix: hold a `WifiManager.MulticastLock` while the service advertises or browses.

**M5. The documented build fails for anyone except the owner.**
Evidence: `project.yml:62` hard-codes `DEVELOPMENT_TEAM: <owner team ID>`; `project.yml:48` and `:69` build for `aarch64-apple-darwin` and `arm64` only. `README.md:27-28` sends readers to task 3, whose command (`manual-checks.md:61`) assumes that team.
Scenario: a reviewer runs the `xcodebuild` line and gets a signing error for a team they do not belong to. On an Intel Mac the Rust library does not match the build architecture.
Fix: read the team from a local, ignored settings file with a documented step, and state Apple silicon only in the README.

**M6. Without adb, or without USB debugging, the cable does nothing and nothing says why.**
Evidence: the engine treats a missing adb as normal (`engine/api/lifecycle.rs:172`) and reports it as `adb_present` (`:405`). The Mac app reads `adbPresent` only in `Support/PreviewData.swift`. The README never says USB needs Developer options, USB debugging, and an installed adb.
Scenario: a first user plugs in the cable, sees no USB badge, and has no message to act on.
Fix: show a line in the device screen when adb is missing or the phone is unauthorized, and list the three requirements in the README.

**M7. The "delta on save" probably does not apply to how Mac apps save.**
Evidence: the delta runs only when the destination already exists (`dav/put.rs:13-20`, `dav/handlers/write.rs:676-689`). `MOVE` is a plain rename (`dav/handlers/write.rs:281-292`). The only test is two `PUT`s to one path (`dav_write.rs:145-182`).
Scenario: TextEdit and most document apps write a temporary file and then rename it over the original. On this mount that is a full new `PUT` plus a `MOVE`, so every byte crosses the wire. Suspected; a log of the verbs WebDAVFS sends during a TextEdit save would prove it.
Fix: log one real app save through the bridge, then either reword the claim or add a delta for a `MOVE` onto an existing file.

### Minor

**m1. Two source addresses still fill every pending handshake slot.**
Evidence: `limits.rs:107-110` and `:138`; the doc comment itself says two addresses can each hold their full share.
Scenario: a device on the trusted Wi-Fi adds a second IPv4 address and holds 32 slots, sending one byte per connection to pass the two second first-byte deadline. No phone can connect while it runs.
Fix: cap per address well below half the total, or reserve slots for addresses that belong to paired peers.

**m2. Four silent local connections still block every new Finder connection.**
Evidence: `dav/server.rs:58` and `:72`.
Scenario: any local process, under any user, reopens four connections every five seconds. Finder's existing connections live on, but a new mount or a new Finder connection is refused.
Fix: accept the unauthenticated connection and close the oldest unauthenticated one, rather than refusing the newest.

**m3. The share-target grant check is one flag for the whole intent, not a proof per URI.** Suspected.
Evidence: `MainActivity.kt:135` reads `FLAG_GRANT_READ_URI_PERMISSION` from the intent; the URIs come from `EXTRA_STREAM` (`ShareIntake.kt:145-167`); a MediaStore URI is resolved to a path with Ferry's own all-files access (`ShareIntake.kt:226-233`).
Scenario: an app sets its own `ClipData` with a URI it may grant, sets the flag, and puts another MediaStore URI in `EXTRA_STREAM`. Android checks only the `ClipData` URI, so Ferry pushes a file the sender could not read to the paired Mac. The file reaches the owner's own Mac, so the harm is small. A test app that sends this intent would prove it.
Fix: read only through `openInputStream` on each URI, or call `checkUriPermission` per URI for the calling package.

**m4. The phone's private key goes into Android backup and is readable through adb.**
Evidence: `AndroidManifest.xml:53` sets `allowBackup="true"` with no exclusion rules; the key is `filesDir/device.key` (`DeviceKeyStore.kt:17`, `:36`); only a debug build exists (`build.gradle.kts:31-32`), and a debug build allows `run-as`.
Scenario: any computer the phone trusts for USB debugging reads the key with `adb shell run-as app.ferry cat files/device.key`. A restore to a new phone clones the old phone's identity.
Fix: exclude `device.key` and the peer store with `dataExtractionRules`, or set `allowBackup="false"`.

**m5. A paired device may delete or overwrite anything in the shared roots, and no screen offers read only.**
Evidence: the Mac defaults to writable Desktop and Downloads (`EngineModel+Settings.swift:81-91`); the phone shares all of shared storage as writable (`FerryEngine.kt:242-247`); the engine supports read only (`roots.rs:258`), but no app sets it. Writes have no quota.
Scenario: a paired phone, or a person who paired a stranger by mistake, deletes the Mac's Desktop files or fills its disk.
Fix: add a read-only switch per root in Settings, and default Desktop to read only.

**m6. The QR dial list falls back to loopback and link-local addresses.**
Evidence: `offer.rs:117-135` drops them "outright", but `engine/pairing.rs:701-711` dials the raw list when the filtered one is empty.
Scenario: a crafted QR code makes the phone connect to its own loopback ports and send a Noise first message. The impact is small.
Fix: fail the scan when the filtered list is empty.

**m7. Files pulled to the Mac carry no quarantine attribute.** Suspected impact.
Evidence: a grep for `quarantine` in `crates/`, `macos/`, and `docs/` finds nothing.
Scenario: an app or script downloaded on the phone lands in `~/Downloads/Ferry` and opens without the Gatekeeper check a browser download gets.
Fix: set `com.apple.quarantine` on each landed file, as AirDrop does.

**m8. The Play Store would likely refuse this manifest, and the manifest still speaks of a Play listing.**
Evidence: `MANAGE_EXTERNAL_STORAGE` (`AndroidManifest.xml:16`), fine location, and a `dataSync` service used for presence each need a declaration Google reviews. `PLAN.md:219-225` cuts the Play Store, but `AndroidManifest.xml:36-38` mentions "the Play listing".
Scenario: a reader asks where to install it and finds only a debug APK installed over adb.
Fix: say in the README that the phone app is sideloaded from source only, and remove the Play wording.

### Attack surface, answered

- An unpaired device on the same Wi-Fi sees a random mDNS name and the protocol version (`discovery.rs:233-253`). Its `KK` handshake always fails against a throwaway key (`engine/inbound.rs:130-151`). It can hold handshake slots (m1) and race a pairing window (security finding 4). It cannot read or write a file.
- A paired peer cannot leave its roots. `RemotePath::parse` refuses `..`, `.`, NUL, and backslash (`path.rs:103-130`), and `cap_std` refuses a symlink out of a root (`localfs.rs:13-25`). List entries with `/` are refused at decode (`ops.rs:473`). Frames, reads, lists, and manifests are capped (`limits.rs:28-73`), and serving connections are capped at eight per peer (`engine/serving.rs:75`).
- The WebDAV bridge binds `127.0.0.1` on a random port (`dav/mod.rs:278`). Every request needs Basic auth with a random password made at each start (`dav/mod.rs:284`) and an exact `Host` match (`dav/server.rs:342-343`). Another user or process on the Mac cannot use it without that password; it can only slow it (m2). The mount sits under `~/Library`, which other users cannot enter.
- Pairing by code uses commit and reveal, as decision 6 says (`noise.rs:352-371`, `:406-426`). QR pairing uses `IK` with a single-use 16 byte nonce and a confirm by name on both sides (`engine/pairing.rs:277-310`). The name is chosen by the other device, so the confirm proves consent, not identity.
- The Mac keeps its key in the Keychain as `WhenUnlockedThisDeviceOnly` (`KeyStore.swift:96`). The phone keeps it in app-private storage (`DeviceKeyStore.kt:25-35`), with the backup gap in m4.

## Prior fixes checked

| Prior finding | Claimed fix | Holds? | Evidence |
|---|---|---|---|
| `fable-security.md` 1, silent connections block pairing | Two second first-byte deadline, per-address cap | Partly | `tcp.rs:255` holds the deadline. The per-address cap became 16 of 32 (`limits.rs:110`, `:138`), so two addresses still fill every slot (m1). |
| `fable-security.md` 3, a scan pairs with no question | Both sides confirm by name before storing | Yes | The scanning side holds the connection (`engine/pairing.rs:231-266`) and stores only in `pair_after_confirm` (`:299-305`). |
| `fable-security.md` 5, unbounded serving connections | Eight per peer, idle write timeout | Yes | `engine/serving.rs:75` and `tcp.rs:370-371`. The uncapped variant is used once, for the pairing connection (`engine/pairing.rs:451`). |
| `fable-security.md` 7, local process blocks the bridge | Five second first head, four unauthenticated slots | Partly | `dav/server.rs:58` and `:72` hold, but four silent connections still block new ones (m2). |
| `third-run-engine.md` 2, a newline in a network name | Refuse control characters | Yes | `networks.rs:139-141` on `add` (`:197`) and on `load` (`:164-166`). |
| `ux-gestures.md` 1, share reads files the sender could not | Require the read grant | Partly | The check is the intent flag only (`MainActivity.kt:135`), so it can be bypassed through `ClipData` (m3, suspected). |
