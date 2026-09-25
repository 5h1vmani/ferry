# Android share grant and lifecycle audit

Date: 26 September 2026
Base: d678d59, then the 14 builder commits up to fa10d74.
Scope: the share target check, commits fa10d74, 3ebd244 and 2aabb7d, and one string.
Method: I read the Kotlin sources and the AOSP sources named below. I ran no app and no device.

## Verdict on the share grant

The concern about fa10d74 is refuted. `Context.checkUriPermission` counts only grants in Android's grant table. `ActivityManagerService.checkUriPermission` calls `UriGrantsManagerService.checkUriPermissionLocked`, and that reads only `mGrantedUriPermissions` for the uid. Android 12 does the same, at `android12-release` `ActivityManagerService.java:5680-5689`. All files access is never an entry in that table. So a URI that Ferry can read only through all files access fails the check.

Dropping the whole-intent flag lost nothing. `Intent.migrateExtraStreamToClipData` adds `FLAG_GRANT_READ_URI_PERMISSION` by itself to any `ACTION_SEND` that has `EXTRA_STREAM` and no ClipData. The flag never said anything about one URI.

What Android allows:

- `Activity.getLaunchedFromUid` (API 34) and `ComponentCaller.getUid` (API 35) return a uid only when the sender opts in with `ActivityOptions.setShareIdentityEnabled(true)`. Apps that share through the share sheet do not opt in. Ferry cannot require it.
- `ComponentCaller.checkContentUriPermission` (API 35) needs no opt-in. Android records, at launch time, what the sender could read among the URIs in `getData`, `EXTRA_STREAM` and ClipData. It counts both provider permissions and grants (`ActivityCallerState.computeCallerInfo`).
- The share sheet does not hide the sender. `ActivityTaskManagerService.startActivityAsCaller` sets the calling uid to the share sheet's own `launchedFromUid`, which is the app the person shared from.
- Android 14 and earlier have no API that names the sender or its access.
- MediaStore declares `forceUriPermissions="true"` in the MediaProvider manifest. So a gallery that shares a MediaStore URI always creates a grant, even though Ferry could read the file anyway. `ExternalStorageProvider` and other documents providers need `MANAGE_DOCUMENTS`, which Ferry does not hold, so a grant is created. A `FileProvider` is not exported, so a grant is created.

Real impact. A shared file lands in `Downloads/Ferry/<name>` on the paired Mac (`docs/engine-contract.md:719-721`). Ferry pushes it at once, with no tap on the phone (`MainActivity.kt:214`). The file goes only to the person's own Mac, never to the sending app.

What I implemented, in `ShareIntake.mayRead` (`ShareIntake.kt:178-188`). A URI passes only when all three rules hold.

1. It is a content URI. A `file://` path is refused.
2. Ferry holds a read grant for this exact URI.
3. On Android 15 and later, the sending app itself could read it (`MainActivity.kt:171-199`).

An API can separate a sender's grant from Ferry's own access, so I did not add a confirmation step. The remaining limits are listed under "Not fixed".

## Findings

**1. A grant left over from an earlier share passed the check (low, fixed on Android 15 and later).**
Evidence: `ShareIntake.kt:178-188` at fa10d74 checked only Ferry's grant table. Grants stay on the activity record until `ActivityRecord.removeFromHistory` calls `removeUriPermissionsLocked`.
Scenario: Photos shares `IMG_1` to Ferry. Later, app X cannot read `IMG_1`. App X sends its own ClipData and puts the URI of `IMG_1` in `EXTRA_STREAM` with no grant. The grant from Photos still exists, so `IMG_1` goes to the Mac a second time.
Change: rule 3 now asks `ComponentCaller.checkContentUriPermission` about the sender. Commit a7e5173.

**2. fa10d74 pushed ClipData URIs as files (low, fixed).**
Evidence: `ShareIntake.kt:156` at fa10d74 added every ClipData URI to the push list.
Scenario: a sender shares a link and adds a preview image in ClipData, as Android's share sheet guide suggests. Ferry sends the preview image to the Mac, and the person never chose it.
Change: `urisFrom` reads `EXTRA_STREAM` only (`ShareIntake.kt:153`). Commit 57d4ff1.

**3. A rotation pushed the same share again (low, fixed).**
Evidence: `MainActivity.kt:110` at d678d59 acted on `getIntent` in every `onCreate`. `AndroidManifest.xml:65-69` declares no `configChanges`.
Scenario: a person shares three photos and then rotates the phone. The three photos go to the Mac again. On Android 15, rule 3 would also refuse a later `onNewIntent` share after a rotation and show a wrong error.
Change: `onCreate` acts only on a fresh launch, not on a restore or a relaunch from Recents (`MainActivity.kt:130`). Commit 587a8cc.

**4. The six-hour limit notice was removed at once (low, fixed).**
Evidence: 2aabb7d `ReachableService.kt:189` posted the notice on `NOTIFICATION_ID`, and `:194` then called `stopForeground(STOP_FOREGROUND_REMOVE)`. `ServiceRecord.cancelNotification` cancels that id with no tag, later, from the system's own handler.
Scenario: on Android 15, the service stops after six hours. The person sees no notice and does not know why the phone is not reachable.
Change: the notice has its own tag (`ReachableService.kt:215`). `onTimeout` now leaves the foreground and calls `stopSelf()` first (`:194-202`). Advertising cancels the notice when it starts again (`:170`). Commit c43dc57.

**5. Approximate location showed as "Not granted" (low, fixed).**
Evidence: 3ebd244 `MainActivity.kt:89` read only the fine grant. Settings showed one word for every answer that was not fine.
Scenario: a person picks "Approximate" in the prompt. Settings says "Not granted", but Android's settings page says "Allowed". The person cannot see that precise location is the missing choice.
Change: `Permissions.locationApproximateOnly` (`Permissions.kt:87`, `:115-117`) and a new Settings word (`strings.xml:342`). Commit 11e12de.

**6. The off notification still said "Not advertising" (low, fixed).**
Evidence: `strings.xml:304` before this change.
Change: it now reads "%1$s is not reachable over Wi-Fi." Commit daa50f2.

## Reviewed, correct as written

- The multicast lock is released on every path: the stop action (`ReachableService.kt:133`), `onTimeout` (`:201`) and `onDestroy` (`:236`). The failed start path never takes the lock and ends in `onDestroy`. Android releases the lock when the process dies.
- `onTimeout(int, int)` is never called before API 35, so the override does nothing there. Android calls it with the last start id (`ActiveServices.onFgsTimeout`). `stopForeground` clears the crash timer (`maybeStopFgsTimeoutLocked`).
- Fine and coarse location are requested together and both are declared (`AndroidManifest.xml:55-56`). A refused or dismissed prompt gives false. `onResume` then reads the real state through `Permissions.refresh`.

## Not fixed

- Android 14 and earlier keep the limit in finding 1. No API names the sender there. The worst case is a second push of a file the person already sent to the same Mac.
- A provider that is exported with no read permission gets no grant, because Android skips a grant when the target already has access (`UriGrantsManagerService.checkGrantUriPermissionUnlocked`). Rule 2 refuses such a share on every version. Ferry's own documents provider is the same case in the picker. I don't know how common such providers are.
- Any app in the foreground can push a file it can read to the Mac with no tap on the phone. This is not a confused deputy, because that app could read the file anyway. Recommendation: add a confirm step on the phone before a share is pushed, because the push lands on another device. This is Shiva's product decision.
