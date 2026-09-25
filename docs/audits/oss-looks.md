# The open-source looks audit

Date: 25 September 2026
Commit: d678d59, on main.
Scope: both apps as a person meets them, and the repo as a first-time visitor meets it. Capability, security, and licensing are out of scope.
Method: read only, from source. Nothing was built, run, rendered, or screenshotted. Contrast ratios come from a python3 script. It applies the WCAG 2.x relative luminance formula to `design/colors.json` through the role map in `design/tokens.json`.

## Verdict

The apps are unusually disciplined underneath. Strings, tokens, and error words each live in one place, and literal drift is close to zero. The public face is not ready: the README gives no image and no install path, and it opens with a dated changelog. The Mac app has no icon, and on the phone dark mode breaks the first screen and every filled button.

## Findings

### Blocker

**B1. The README's first screen does not say what Ferry looks like, whether it works, or how to get it.**
- Evidence: `README.md:5-28`. The status is one 20-line paragraph of dates and pass counts. `README.md:27-28` sends a visitor to `docs/manual-checks.md` task 3, which opens with "copy what you see and send it back" (`docs/manual-checks.md:9-10`). The repo tracks zero images and has no badge.
- Visitor sees: a good one-line pitch, then a changelog. In paragraph three they learn that nothing is packaged and most screens have not run on a device.
- Fix: rewrite the top of the README to the outline below, and move the dated history to a CHANGELOG file.

**B2. The Mac app has no icon, and the logo never reached either app.**
- Evidence: `macos/project.yml:19-26` has no asset catalog, and no `.xcassets` exists under `macos/`. `android/app/src/main/res/drawable/ic_launcher_foreground.xml:9-14` draws two placeholder rectangles. The designed mark is in `design_ouput/logo_assets/ferry-app-icon.svg`, which is ignored by git.
- User sees: the blank generic app icon in the Dock, in Cmd-Tab, and on every notification. Any README screenshot will show it.
- Fix: add `Assets.xcassets/AppIcon` from `ferry-app-icon.svg`, and build the Android adaptive icon from `ferry-mark.svg`, with a monochrome layer.

**B3. The phone's first-run screen is unreadable in dark mode.**
- Evidence: `FirstRunScreen.kt:54-58` draws no background. The activity theme is `@android:style/Theme.Material.Light.NoActionBar` (`AndroidManifest.xml:56,62`), so the window stays light. The text uses the dark token `#eeeef0` (`Tokens.kt:44`).
- User sees: near-white text on a near-white window, 1.11 to 1. It is the first screen a dark-mode user sees.
- Fix: give `FirstRunScreen` a `Scaffold` or `Surface` with `FerryColor.background()`, and move the manifest theme to a DayNight theme.

**B4. Every filled button and the presence switch use Material's default purple `onPrimary` in dark mode.**
- Evidence: `FerryApp.kt:79-85` overrides only `primary`, `background`, `surface`, and `onSurface`. `darkColorScheme()` keeps `onPrimary = #381E72`. Filled buttons at `EmptyState.kt:40`, `PairingCode.kt:67`, `PairingScreen.kt:212,348-353`, and `FirstRunScreen.kt:91-96` take their label colour from it. The switch thumb at `PresenceControl.kt:98-102` does too.
- User sees: dark purple labels on a blue button, 2.27 to 1, on Pair, Confirm, Grant access, and Scan. Other baseline purple greys show on outlines and the unchecked switch.
- Fix: generate a full light and dark `ColorScheme` in `Tokens.kt` from the token roles, with `onPrimary` set to `on_accent`.

### Major

**M1. The main presence control is called "Advertising" on both apps.**
- Evidence: `macos/Ferry/Strings.swift:51-52`, `strings.xml:92-97`. The word is the mDNS term. docs/voice.md rule 4 asks for real names.
- User sees: a switch that seems to be about ads, on the home screen of both apps and in the notification.
- Fix: rename the states to plain words such as "Visible on Wi-Fi" and "Hidden on Wi-Fi", on both apps at once.

**M2. Forgetting a device has no confirmation on either app.**
- Evidence: `DeviceRow.swift:68-70` (context menu), `DeviceDetail.swift:120-122`, `SettingsScreen.kt:178` with `FerryApp.kt:267-270`.
- User sees: one click in a context menu ends the pairing. Getting it back needs both devices and a new pairing.
- Fix: add a `confirmationDialog` on the Mac and an `AlertDialog` on the phone that name the device.

**M3. TalkBack cannot switch presence on or off.**
- Evidence: `PresenceControl.kt:65` applies `clearAndSetSemantics` to the whole `ListItem`, which removes the `Switch`'s toggle action at lines 98-102.
- User sees: TalkBack reads the state but has no action on it. The phone's main control does not work for a screen-reader user.
- Fix: make the `ListItem` `toggleable` with `Role.Switch` and pass `onCheckedChange = null` to the inner `Switch`.

**M4. "Open Ferry" in the menu bar does not reopen a closed window.**
- Evidence: `MenuBarPresence.swift:53-55` calls `NSApplication.shared.activate(ignoringOtherApps: true)`. That call brings the app forward but creates no window. The scene is a `WindowGroup` with no id (`FerryApp.swift:22`).
- User sees: after closing the window, the menu's only navigation control appears to do nothing.
- Fix: give the scene an id and call the `openWindow` environment action from the menu.

**M5. The cable path needs developer mode on the phone and adb on the Mac, and neither app says so before it fails.**
- Evidence: the pairing sheet says "Plug in a cable" (`Strings.swift:370`). The requirement appears only in error words (`design/errors.json:14-17`) and in `docs/manual-checks.md:40-46`. The README table at `README.md:61-68` does not state it.
- User sees: a cable that does nothing, then "USB is unavailable. adb was not found on this Mac."
- Fix: state both requirements in the README and on the Mac's waiting screen, next to the cable line.

**M6. Both apps state false facts until the engine starts.**
- Evidence: the Mac starts with `devices = []` and `presence = .unknown`, which means not advertising (`EngineModel.swift:54,58`, `Snapshot.swift:53-59`). The phone starts with empty flows (`FerryEngine.kt:74,130`); `_started` at line 163 is never read by a screen. `docs/ia.md:209` claims there is no loading state.
- User sees: "No phone paired", a prominent "Pair a phone", and a boxed "Not advertising" warning. This shows on every launch, even when a phone is paired.
- Fix: expose a started flag and show an empty sidebar and no presence line until the first snapshot arrives.

**M7. Every phone notification shows the system upload arrow.**
- Evidence: `ReachableService.kt:222,242,271` and `TransferNotifier.kt:106` use `android.R.drawable.stat_sys_upload`.
- User sees: a permanent upload arrow in the status bar whenever Ferry is reachable, and the same arrow for downloads.
- Fix: add a one-colour notification icon made from the Ferry mark.

**M8. Dark mode uses `accent` as a foreground where it fails contrast.**
- Evidence: `ErrorBlock.kt:81` sets the Retry label to `accent` on `surface_raised`, 2.72 to 1. `PresenceControl.kt:101,108` puts the `accent` track on an `accent_surface` row, 2.59 to 1. `TransferRow.kt:64-65` draws the active bar in `accent` on a `border` track, 1.96 to 1.
- User sees: a faint Retry label, a switch that barely shows it is on, and a progress bar that is hard to see.
- Fix: use `accent_text` for any text or icon, and use a lighter accent step for bars and tracks in dark mode.

**M9. The Mac access log can put up to 1000 rows above "Forget this phone".**
- Evidence: `EngineModel+Devices.swift:74-77` asks for `limit: 1000`. `DeviceDetail.swift:75-81` renders every row inside the device's `Form`, before the Info footer.
- User sees: after one Finder browse of DCIM, a very long pane. The Info facts and Forget are far below.
- Fix: show the last 20 rows in the section and put the full log in its own window or sheet.

**M10. Several visible strings read like notes between developers.**
- Evidence: `strings.xml:312` says "Not editable in phase 1." `strings.xml:187` and `Strings.swift:40-44` say "Report this code." with no place to report it. `design/errors.json:21,36,246,366` mention BLAKE3, a NUL character, a relative adb path, and 1 MiB writes.
- User sees: internal project phases and hash names in Settings and in errors.
- Fix: rewrite the flagged strings in the copy sample below, and link "Report this code" to the GitHub issues page.

**M11. docs/ reads as a working notebook, not as public documentation.**
- Evidence: 45 files and about 10,000 lines. Internal notes include `docs/agent-runs.md:1-7`, `docs/manual-checks.md:1-12`, `docs/ux-plan.md`, `docs/ux-fix-plan.md:3-4`, `docs/engine-contract.md` (1191 lines), the 12 files in `docs/audits/`, and `docs/design-pass/` (a canvas and a 1911-line `support.js`). `PLAN.md:14-17` calls the project "not a public product".
- Visitor sees: no index, and no way to tell reference from scratch work.
- Fix: keep `protocol.md`, `decisions/`, `jobs.md`, `voice.md`, `design.md`, `components.md`, `ia.md`, and `toolchain.md` at the top. Move the rest to `docs/internal/`, and add a `docs/README.md` index.

**M12. A visitor cannot build the Mac app without editing the project file.**
- Evidence: `macos/project.yml:60-62` pins the owner's `DEVELOPMENT_TEAM: L84QUBJX67`. `project.yml:69` builds arm64 only, and `android/app/build.gradle.kts:14,24` sets Android 12 and arm64 only. The README states none of this.
- Visitor sees: a signing error on the first build, with no requirements listed.
- Fix: read the team from an ignored local `.xcconfig`, and list the platform and architecture requirements in the README.

### Minor

- **m1. The Mac TransportBadge has no "Connecting" state.** Evidence: `docs/components.md:32`. The phone has it (`strings.xml:72`); `TransportBadge.swift:7-13` does not. Sees: the two apps disagree while a link comes up. Fix: add the state and its string on the Mac.
- **m2. The Mac Info footer shows two facts where the IA says four.** Evidence: `docs/ia.md:118`, `DeviceDetail.swift:113-119`. "Key fingerprint" shows the full 64-character key. Sees: a raw hex string labelled as a fingerprint. Fix: show the same short fingerprint the phone shows, and decide on the four facts.
- **m3. The empty Mac window says "No phone paired" twice and offers "Pair a phone" twice.** Evidence: `DevicesSidebar.swift:21-22,39-44`, `ContentView.swift:52-53`. The phone refuses this on purpose (`DevicesScreen.kt:120-122`). Fix: show one Pair control in the empty state.
- **m4. The Mac scene is a `WindowGroup`, so File > New Window opens duplicates.** Evidence: `FerryApp.swift:22`; `docs/ia.md:97` says one window. Fix: use a single `Window` scene.
- **m5. Return and Escape do nothing special in the pairing sheet.** Evidence: no `.keyboardShortcut(.defaultAction)` or `.cancelAction` in `PairingQRView.swift:127-132` or `PairingCode.swift:30-36`. Fix: mark Pair and Confirm as default and Cancel or Refuse as cancel.
- **m6. The menu bar dropdown has no Settings or Quit item, and VoiceOver reads its icon only as "Ferry".** Evidence: `MenuBarPresence.swift:21-61,88`. Fix: add both items and put the presence state in the label.
- **m7. Pairing failure on the Mac always restarts the scan method.** Evidence: `PairingSheet.swift:127-130`. `docs/ia.md:220` names the control "Show a new code", and the code shows "Retry". Fix: retry the method that failed, with the contract's words.
- **m8. The Mac shows an iPhone glyph for Android phones.** Evidence: `design/tokens.json:34` maps `device_phone` to `iphone`, used in `DeviceRow.swift:17` and `PairingQRView.swift:113`. Fix: map it to a generic phone symbol.
- **m9. Byte units differ between the apps and from the contract.** Evidence: the Mac prints raw "bytes" under 1 MB (`Format.swift:27-36`); the phone prints "kB" (`strings.xml:111`); `docs/components.md:97` shows "48 KB". Fix: one unit rule, applied in both formatters.
- **m10. The paused bar is almost invisible.** Evidence: `TransferRow.kt:83-84` draws `border_strong` on a `border` track, 1.36 to 1 light and 1.82 to 1 dark. The Mac tints the same bar `borderStrong` (`TransferRow.swift:86`). Fix: keep the bar in a muted accent step and let the "Paused." text carry the state.
- **m11. The phone Forget button is coloured as plain text.** Evidence: `SettingsScreen.kt:181-183` uses `FerryColor.text()`. The comment above it and `design/tokens.json:17` say destructive uses Material's error colour. Fix: use `MaterialTheme.colorScheme.error`.
- **m12. The phone pairing code bypasses the display token, and the scan frame differs from the IA.** Evidence: `PairingCode.kt:46` uses `mono` at `40.sp`; `PairingScreen.kt:428` sets 240dp, and `docs/ia.md:248` says 200. Fix: use `FerryFont.display()` with tabular digits, and align the number.
- **m13. Phone Settings shows the name as fixed text, but the IA says it is editable.** Evidence: `docs/ia.md:393`, `SettingsScreen.kt:98-104`. Fix: update the IA or build the field.
- **m14. Buttons with a fixed 48dp height will clip text at large font sizes.** Evidence: `FirstRunScreen.kt:95,105`, `PairingScreen.kt:352,421`. Fix: use `heightIn(min = 48.dp)`.
- **m15. The chunk disclosure row is below 48dp and reports no expanded state.** Evidence: `ChunkDisclosure.kt:49-53`, a `clickable` row with 8dp vertical padding. Fix: add `minimumInteractiveComponentSize()` and an expanded `stateDescription`.
- **m16. The Android icon is clipped by a circle mask and has no monochrome layer.** Evidence: `ic_launcher_foreground.xml:11-14`. The outer corners reach about 40.6dp from the centre; a circle mask keeps 36dp. `mipmap-anydpi-v26/ic_launcher.xml:2-5` has no `<monochrome>`. Fix: redraw from the mark inside the 66dp safe zone.
- **m17. The phone has no edge-to-edge setup and no screen transitions.** Evidence: `MainActivity.kt:95` calls `setContent` without `enableEdgeToEdge()`. `FerryApp.kt:191` switches screens with a bare `when`. Fix: call `enableEdgeToEdge()`, pad `FirstRunScreen` by the system bars, and add a platform transition.
- **m18. The contract contradicts itself on greys.** Evidence: `docs/design.md:24` says neutrals are the platform's own greys; `design/tokens.json:18-24` uses the Radix gray scale, which the Mac draws over system backgrounds. Fix: change the sentence to match the tokens.
- **m19. The README decision list stops at 0009.** Evidence: `README.md:81-89`; `docs/decisions/0010-no-mac-sandbox.md` and `0011-gestures-not-a-file-manager.md` exist. Fix: list all eleven.

### Supporting counts

The literal counts cover shipping code only. Previews, zero values, and 1pt hairlines are left out.

| App | Spacing off the scale | Fixed frame sizes | Font sizes | Colours | Worst files |
|---|---|---|---|---|---|
| Mac | 2 (`DeviceRow.swift:20`, `SettingsView.swift:142`) | 8 | 2 (`PairingQRView.swift:114`, `PairingCode.swift:42`) | 1, justified (white behind the QR code) | `PairingQRView.swift` 4, `PairingSheet.swift` 2, `DeviceRow.swift` 2 |
| Phone | 0 | 2 (`PairingScreen.kt:381,428`) | 1 (`PairingCode.kt:46`) | 3, justified (camera overlay) | `PairingScreen.kt` 5, `PairingCode.kt` 1 |

The phone also uses the spacing token `FerrySpace.s7` as a button height or icon size six times. No user-facing string is inline in a view on either app. No string uses a word that voice.md bans.

The copy sample holds 30 strings: 15 from `Strings.swift`, 8 from `strings.xml`, and 7 from `design/errors.json`. 13 pass and 17 fail. Ten fails are on screens every person sees:
- "Advertising" and "Ferry is quiet on this network."
- "Show chunks", and "Key fingerprint" over a full key.
- "Not editable in phase 1.", "Ferry has no words for the code", and "Report this code."
- "Record" as a Settings group, and the fragment "Location, so Ferry can read the Wi-Fi name."
- "One notification says Ferry is reachable, and stops it."

Seven fails are in rare errors. They are the BLAKE3, NUL, relative-path, 1 MiB, open-mode, and stored-format lines, and "The device is not reachable." with no name.

The table below lists the states by screen. A dash means the contract says the state does not apply.

| Screen | Empty | Loading | Error | First run |
|---|---|---|---|---|
| Mac Devices | yes | missing (M6) | yes, engine start | yes, but no word on permissions or adb (M5) |
| Mac Detail | yes | missing | yes, but a drop error stays until the device changes (`DeviceDetail.swift:43-45,86-88`) | - |
| Mac Settings, Networks | missing: an empty section when no name is known and none is trusted | missing | yes | - |
| Mac Menu bar | yes | missing | none shown | - |
| Phone Devices | yes | missing (M6) | yes | yes, broken in dark mode (B3) |
| Phone Pairing | - | yes, "Starting." | yes | camera and location prompts are explained |
| Phone Settings, Access log | yes | retention line waits | none needed | - |

On platform fit, the Mac has a menu bar extra, a Settings scene, Cmd-O, Cmd-R, context menus, a Services entry, Dock drop, and a Dock badge. It lacks a toolbar, a single window, and default keys in the sheet. The phone uses Material 3 components throughout. It lacks a DayNight theme, a full colour scheme, edge-to-edge setup, and screen transitions. Dynamic colour is off on purpose (`docs/design.md:14-22`), so it is not a finding.

## Contrast table

The WCAG thresholds are 4.5 to 1 for body text, and 3 to 1 for large text, icons, and control edges.

| Foreground on background | Light | Dark | Result |
|---|---|---|---|
| text on background | 16.04 | 16.28 | pass |
| text on surface_raised | 14.43 | 13.57 | pass |
| text_secondary on background | 5.82 | 9.05 | pass |
| text_secondary on surface_raised | 5.23 | 7.55 | pass |
| text_secondary on accent_surface | 5.30 | 7.19 | pass |
| accent_text on background | 5.24 | 9.69 | pass |
| accent_text on surface_raised | 4.72 | 8.08 | pass, light is close to the limit |
| accent on background | 5.64 | 3.26 | dark fails for text, passes for icons |
| accent on surface | 5.50 | 3.03 | dark fails for text, icons are at the limit |
| accent on surface_raised | 5.08 | 2.72 | dark fails for text and icons (M8) |
| accent on accent_surface | 5.14 | 2.59 | dark fails 3 to 1 (M8) |
| on_accent on accent | 5.79 | 5.79 | pass |
| Material default dark onPrimary `#381E72` on accent | n/a | 2.27 | fail (B4) |
| dark text `#eeeef0` on the light platform window `#fafafa` | n/a | 1.11 | fail (B3) |
| accent bar on border track | 4.11 | 1.96 | dark fails (M8) |
| border_strong bar on border track | 1.36 | 1.82 | fails (m10) |
| border_strong edge on surface | 1.82 | 2.82 | fails if the edge carries meaning |

Every pair of text roles passes. The failures come from using `accent`, step 9, as a foreground in dark mode, and from colours the tokens do not control.

## README rewrite outline

- **Ferry**: the one-line pitch, a CI badge from `.github/workflows/ci.yml`, and a licence badge.
- **Screenshot**: one image of the phone open in Finder beside the Mac window, taken after B2 is fixed.
- **Status**: one sentence, "Pre-release. Build from source. Not yet packaged.", with the date of the last device test.
- **What works today**: a table of features by Mac, phone, and "run on a real device: yes or no".
- **Why Ferry**: three lines on Android File Transfer's end, and the Finder gap LocalSend leaves.
- **Compared with alternatives**: a table of LocalSend, OpenMTP, Android File Transfer, and MacDroid. Columns are Finder browse, Wi-Fi, USB, resume, open source, and price. Verify each cell on the vendor's page.
- **Requirements**: macOS 14 on Apple silicon, Android 12 on arm64, and for USB, adb plus USB debugging.
- **Build from source**: the Mac steps with your own signing team, and the phone steps with `./gradlew`; link `docs/toolchain.md`.
- **Quick start**: pair by QR code, send a file, and open the phone in Finder, in four steps each.
- **How it works**: the two-layer section as it is now, with the 4718 cut points line and its test file.
- **Security model**: one paragraph with links to decisions 0003 and 0006.
- **Design decisions**: all eleven records.
- **Documentation**: a link to a new `docs/README.md` index.
- **Licence**: MIT.

## Look list for the owner

Each item needs a real device, because source cannot settle it.

1. Phone in dark mode, first launch: check that the first-run text and the Grant access label read clearly (B3, B4).
2. Phone in dark mode, Devices: check the presence switch thumb against its track, and the Pair label against its button (B4, M8).
3. Mac Dock, Cmd-Tab, and a transfer notification, after B2: check that the icon holds its shape at 16 and 32 points.
4. Pixel launcher with the circle icon shape: check whether the icon's outer corners are cut, and whether the two shapes read as two (m16).
5. Mac cold launch with a phone already paired: check for a flash of "No phone paired" or "Not advertising", and time it (M6).
6. Mac menu bar: close the window, then choose "Open Ferry"; check whether a window appears (M4).
7. Phone Transfers in dark mode during a real copy: check that the active bar is visible. Then pull the cable and check the paused bar (M8, m10).
8. Mac after browsing DCIM in Finder: scroll the detail pane and check how far "Forget this phone" has moved (M9).
9. Phone on Android 14 or later at the largest font size: check the Grant access, Skip, and Scan labels for clipping. Check whether the pairing code wraps (m14, m12).
10. Phone on Android 15 or later, light mode: check that status bar icons show over the top bar. Check that Skip sits above the navigation bar (m17).
11. Mac with VoiceOver: check that the sidebar presence toggle is announced as a switch and flips with VO-Space. Then check the phone row with TalkBack (M3).
12. Mac text size: set System Settings, Accessibility, Display, Text size to its largest, and check whether Ferry's text changes, as `docs/design.md:29-30` promises.

ALERT: `EngineModel+Devices.swift:74-77` calls the engine for up to 1000 access log rows from inside the `DeviceDetail` view body. That call repeats on every model change, including the 250 ms transfer events. This is a performance question outside this audit.
ALERT: `macos/Ferry/Support/PreviewData.swift:191-209` holds the owner's home path. It ships in public source.
