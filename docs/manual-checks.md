# Checks that need a person at the keyboard

Both questions that needed a person are answered. This file stays as the
record of what was checked and what it showed.

A third task about FSKit used to sit here. It is answered and removed. Public
bug reports run to June 2026 and no source confirms a fix, so the Finder mount
stays on WebDAV. See decision record 8 for the sources.

For each one, copy what you see and send it back. There is a short "what to
send me" list at the end of every task.

---

## Task 1: does USB tethering give the Mac a network connection?

**Answered on 10 September 2026. No.**

The phone is a Pixel 3 XL on Android 12. Android 12 tethers over RNDIS, and
macOS has no RNDIS driver, so no network port appears. Newer Android versions
tether over NCM, which macOS can drive, so a newer phone might pass. This is
the phone the app is for, so the plan builds the USB path properly.

Nothing more to do here.

## Task 2: confirm which Apple team you have

**Answered on 10 September 2026.** The account shows a Personal Team. That is
the free tier, and Apple's capability table says what it can do. See
`PLAN.md` section 7.

Nothing more to do here.

## Task 3: run Ferry on the Mac and the phone

Both apps are built. Nothing but a real run can prove that pairing, the file
copy, and the cable work. Expect rough edges. Every one you find is a result.

### Part A: install the phone app

1. On the phone, open Settings, then About phone. Tap Build number seven
   times. The phone says you are now a developer.
2. Go back to Settings, then System, then Developer options. Turn on USB
   debugging. Ferry needs this for the cable, so it stays on.
3. Plug the phone into the Mac with the cable. The phone asks "Allow USB
   debugging?" Tick "Always allow from this computer" and tap Allow.
4. Build the phone app as the README's "Build from source" section says,
   then install it:

```bash
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

   The line should say `Success`. If it says `no devices`, unplug and plug
   the cable again and check step 3.

### Part B: build and open the Mac app

Build the Mac app as the README's "Build from source" section says, then
open it:

```bash
open macos/build/Build/Products/Debug/Ferry.app
```

The first build takes a few minutes. Two system prompts may appear: one
about the local network, and one from the firewall about incoming
connections. Allow both. Ferry cannot find or hear the phone without them.

### Part C: pair over Wi-Fi

Unplug the cable for this part. Both devices must be on the same Wi-Fi.

1. On the phone, open Ferry. The first screen explains two grants. Tap
   Continue, grant all files access, and come back. Devices shows "No Mac
   paired."
2. On the phone, tap Pair. It shows "Pairing on. Open Ferry on the Mac." and
   a four character code. Write that code down.
3. On the Mac, click Pair a phone. It says "Looking for a phone." Within a
   few seconds the phone should appear as "Phone on Wi-Fi" with the same four
   characters. Pick it.
4. Both screens now show six digits. Check they match. Confirm on both.
5. Both device lists should show the other device, reachable over Wi-Fi.

### Part D: copy a file

1. On the Mac, select the phone. The Access line shows where it is mounted.
   Click "Open in Finder" and go into `DCIM`, then `Camera`.
2. Right-click a photo, choose Services, then "Send with Ferry".
3. The transfer line should move and end in Done. The file should be in
   `~/Downloads/Ferry`. Open it and check it is a whole photo.

### Part E: the cable

1. Plug the cable in. Within a few seconds the phone's badge on the Mac
   should say USB.
2. Turn off Wi-Fi on the phone. The badge should still say USB.
3. Copy another photo. It should complete over the cable.
4. Turn Wi-Fi back on.

### What to send back

For each part, one line: worked, or what you saw instead. Paste the exact
words from any error block. If a step does not match what you see, stop
there and describe the screen in words.

**Answered on 10 September 2026.** All five parts worked. Two things were
fixed on the way. The Mac app looked for `adb` in Homebrew's bin folder,
where it is not; it now looks in every known place. And the app was signed
ad hoc, so every rebuild was a new app to the Keychain and asked for the
login password; it is now signed with the owner's Apple team, which needed
one step in Xcode to choose the team.

Nothing more to do here.

---

## Task 4: the designed screens

**Open.** Added 11 September 2026. The Mac app took its designed screens
and the engine gained what they read. Nothing below has run on a real
phone yet. Build both apps as in task 3, pair, and then check each part.

### Part A: presence and the menu bar

1. The Ferry icon sits in the menu bar. Click it. It should say whether
   the phone can find this Mac, and the switch should match the one at the
   bottom of the Devices sidebar.
2. Turn the switch off. Within a few seconds the phone's device list
   should show the Mac as not reachable. Turn it back on.

### Part B: shared folders

1. Open Settings. It should list Desktop and Downloads as shared, and say
   pulled files land in `~/Downloads/Ferry`.
2. On the phone, open the Mac. The first listing should show exactly two
   folders, Desktop and Downloads.
3. Add a folder in Settings. Without restarting anything, list the Mac
   from the phone again. The new folder should appear.
4. Remove it again. The last folder cannot be removed; the button should
   be disabled when one is left.

### Part C: a folder copy

1. On the Mac, open the phone's volume in Finder, go into `DCIM`,
   right-click the `Camera` folder, choose Services, then "Send with Ferry".
2. The Transfers section should show one row, "Camera", counting files up
   and bytes up, with a speed. The files should land in
   `~/Downloads/Ferry/Camera/` with their names kept.
3. Pull the cable or turn Wi-Fi off half way. The row should pause, then
   resume on the other transport, and end Done with every file present.
4. Quit the Mac app half way through a copy and open it again. The row
   should still show the same batch, with the files already done still
   counted.

### Part D: the access log

1. On the phone's device screen on the Mac, scroll to the access log. After
   parts B and C it should show, grouped by day, that the phone listed the
   Mac's folders, and that this Mac read the Camera folder with a file
   count.
2. On the phone, open a file the Mac shares. The Mac's log should gain a
   "read" line for it within a second.

### Part E: Finder

Added for the I1 fix pass. The bridge in `crates/ferry-runtime/src/dav/`
turns the phone's shared folders into a volume Finder can mount, per
`docs/engine-contract.md`, item 6.

1. With the phone reachable, wait a few seconds. A new volume should
   appear in Finder, named after the phone.
2. Open the volume. Finder should list the phone's shared roots, the same
   folders the phone screen on the Mac already shows.
3. Open a folder and click into it. It should list files and folders the
   same way any other Finder window does.
4. Open a photo. It should preview in Finder the normal way, such as with
   Quick Look.
5. On the phone's device screen on the Mac, check the access log. It
   should show a "list" line for the folder from step 3, from opening it
   in Finder.
6. Eject the volume in Finder. It should disappear. Wait for the phone to
   go unreachable and reachable again, such as by turning Wi-Fi off and
   back on. The volume should come back at the same place.

Steps 7 to 11 were added for the I2 fix pass, saving.

7. Open TextEdit. Write a short note, then save it onto the volume, into a
   folder the phone shares. The save should complete without an error, the
   same way it does to any other disk.
8. On the phone's device screen on the Mac, check the access log. It
   should show a "write" line for the new file.
9. In Finder, rename the file you just saved. The rename should complete
   without an error, and Finder should show the new name at once.
10. Make a new folder inside the volume, then delete it. Both should
    complete without an error, and the folder should be gone from the
    Finder window right after the delete.
11. On the phone's device screen on the Mac, check the access log again.
    It should show a "rename" line for step 9 and a "delete" line for
    step 10, alongside the "write" line from step 8.

### Part F: the phone's screens

Added 11 September 2026, for the phone's own designed screens.

1. On the phone, open Ferry. Devices should show the paired Mac,
   reachable, with its transport badge.
2. Start a copy between the two devices. The phone's Devices screen
   should show a Transfers row under the Mac, with a bar and a speed.
3. Open Settings on the phone. It should list this phone's name and
   shared storage, then All files access, Notifications, and Location
   under Permissions.
4. Tap Access log. It should list, grouped by day, what this phone
   served to the Mac and what it read from it.
5. Under Paired, it should show the Mac's name, its key fingerprint, and
   "Forget this Mac".
6. Pull down the notification shade and tap Stop on Ferry's notification.
   The notification should stay, now saying "Not advertising" with one
   action, "Start advertising". Tap it. Devices should show the switch on
   again, and the Mac should see the phone within a few seconds.

### Part G: pairing by scan, from the phone

Added 11 September 2026. This is `docs/engine-contract.md` item 12, the
phone's half.

1. Forget on both sides: on the phone, "Forget this Mac" in Settings; on
   the Mac, "Forget this phone" on the phone's screen. A device that one
   side still holds refuses to pair again.
2. On the phone, tap Pair. The first time, a line says "Location, so
   Ferry can read the Wi-Fi name." and Android asks for location. Allow
   it. Then tap "Scan the Mac's code". On the Mac, click "Pair a phone";
   the sheet shows the square code and "Scan this with the phone."
3. Grant the camera the first time the phone asks for it. The phone
   should show "Point the camera at the Mac's screen."
4. Point the camera at the Mac's code. The Mac should show the phone's
   name and ask whether to pair it. Confirm on the Mac.
5. The phone should then show the Mac's name and "Is this your Mac?"
   with Confirm and Cancel. Tap Confirm.
6. Devices on both should show the other device, reachable. Settings on
   both should list the current Wi-Fi network under Networks.

### What to send back

For each part, one line: worked, or what you saw instead. Paste the exact
words from any error block.

---

## Task 5: the Mac's folders in the phone's Files app

**Open.** Added 11 September 2026. This is job 8 and
`docs/engine-contract.md` item 19. The phone now offers every paired Mac
to the Files app and to every app's open and save dialog. Nothing below
has run on a real phone yet.

Build and install both apps as in task 3. Pair the two devices. Leave the
Mac app open, so the phone can reach it.

1. On the phone, open Files. Open the menu at the top left. The Mac
   should be listed there by its own name. Under the name you should see
   "Reachable over Wi-Fi", or "Reachable over USB" with the cable in.
   Tap the Mac. The screen should list the Mac's shared folders, and
   nothing else. Those are the same folders the Mac's Settings lists.
2. Open one of those folders. It should list the files and folders inside
   it. Each file should show a size and a date. Open a folder inside it.
   That should list its contents the same way.
3. Open a photo. It should appear in the Files app viewer. There should
   be no download step and no copy step. Now open Ferry on the phone, go
   to Settings, and tap Access log. It should hold a line saying this
   phone read that photo.
4. Open another app that saves a file, such as a notes app or a camera.
   Choose its share or save action and pick Ferry's Mac. Pick a folder
   inside one of the shared folders, then save. The save should finish
   with no error. The file should appear in that folder on the Mac. The
   phone's access log should gain a line saying this phone wrote it.

### What to send back

For each step, one line: worked, or what you saw instead. Paste the exact
words from any error block.

---

## Task 6: trusted networks, both apps

**Open.** Added 11 September 2026. This is `docs/engine-contract.md` item
18. Wi-Fi presence should hold only for a network a paired device has
joined before. Nothing below has run on a real phone yet.

Build and install both apps as in task 3.

1. Pair the phone and the Mac while both are on your home Wi-Fi.
2. Open Settings on the Mac. The Networks section should list the home
   network among the trusted names.
3. Open Settings on the phone. The same Networks section should list the
   home network too.
4. On the Mac, remove the home network from the trusted list. The Mac's
   presence control should say "Ferry is quiet on this network."
5. On the phone, wait a few seconds. The Mac should show as not
   reachable in the phone's device list. With the cable in it should
   show as reachable over USB, because the cable is never gated.
6. On the Mac, add the home network to the trusted list again by hand.
   The Mac should become reachable to the phone again within a few
   seconds.
7. On the phone, open Android's Settings, Apps, Ferry, Permissions, and
   set Location to "Don't allow". Reopen Ferry. The presence row should
   say Ferry cannot read the network name, and the Mac should show the
   phone as not reachable over Wi-Fi; USB still works. Settings on the
   phone should offer a way to the permission. Allow location again and
   both should recover within a few seconds. This is the designed rule:
   an unknown network with a trusted list is quiet. A phone that refused
   location at its very first pairing has an empty list and sees no
   change at all.

### What to send back

For each step, one line: worked, or what you saw instead. Paste the exact
words from any error block.

## Task 7: share, drop, and notify

**Open.** Added 16 September 2026. This is `docs/ux-fix-plan.md`, items 1
to 5. Nothing below has run on a real device yet.

Build and install both apps as in task 3. Pair them and keep both on the
same Wi-Fi.

### Part A: the phone shares to the Mac

1. On the phone, open Photos, pick one photo, and share it to Ferry. It
   should appear under the Mac in Transfers and land in
   `~/Downloads/Ferry` on the Mac.
2. Share two files at once. They should appear as one batch.
3. On Devices, tap "Send files", pick a document, and confirm it lands the
   same way.
4. Turn the Mac's Wi-Fi off and share a file. The phone should show an
   error block that says the Mac is not reachable, and nothing should be
   queued.
5. Share a file from an app that is not a gallery, for example a file
   from Chrome's downloads. It should still send. After it ends Done, the
   folder `share` inside Ferry's cache should be empty. Android's Settings,
   Apps, Ferry, Storage shows the cache size.
6. Forget the Mac on the phone, then share a file. Devices should open in
   its empty state and nothing should be sent.

### Part B: the phone's transfer notification

7. Start a large push from the phone, then leave the app with advertising
   on. A second notification should show a moving progress bar.
8. Let it finish. The notification should change to the done line with
   files, size, and duration.
9. Turn Wi-Fi off in the middle of a push so it fails. The notification
   should show what stopped, with a Retry action. Turn Wi-Fi on and tap
   Retry. The push should continue.
10. The "reachable" notification should keep its words and actions
    through all of this.

### Part C: the Mac notifies

11. Start any transfer. macOS should ask once for notification permission.
    Allow it.
12. Let a transfer finish. A notification should name the file or folder
    and show bytes and duration.
13. Make a transfer fail. A notification should show the first line of
    the error.
14. While a transfer runs, the Dock icon should show a badge with the
    running count. It should clear when nothing runs.
15. Open the menu bar item while a batch runs. It should show one line per
    running batch: label, progress bar, speed.

### Part D: the Mac drops and sends

16. Drag a file from Finder onto the device row in the sidebar, then
    another onto the detail pane. Both should start a push into
    `Download` on the phone.
17. Drag a folder onto the device row. It should be refused with a
    three-part error, not dropped silently.
18. Drop a file on the Ferry Dock icon. It should push the same way.
19. Right-click the device row. "Open in Finder" should appear only while
    the phone is mounted. "Send files…" and "Forget" should appear and
    work.
20. Use "Send files…" from the row, from the app menu, and with Cmd+O. All
    three should open the same panel and land in the same folder.
21. The caption at the top of the detail pane should say where a drop
    lands and name the phone, and only while the phone is reachable.

### Part E: the Finder service

22. In Finder, open the phone's mounted volume, select a file, right-click,
    and choose Services, "Send with Ferry". It should appear in Transfers
    as a pull, not as a Finder copy.
23. Do the same on a folder inside the mount. It should appear as one
    batch.
24. Select a file on the Mac's own disk and use the same service. It
    should push to the phone.
25. Right-click a finished pull in Transfers. "Reveal in Finder" should
    appear only while the file is still on disk, and should open it.
26. Right-click a failed row. "Retry" should appear and work. Select the
    device and press Cmd+R. Every failed transfer of that device should
    retry.

If the service does not appear in the menu, open System Settings,
Keyboard, Keyboard Shortcuts, Services, and check "Send with Ferry" under
Files and Folders. A Debug build may need one launch before macOS lists
it.

### What to send back

For each step, one line: worked, or what you saw instead. Paste the exact
words from any error block or notification.

## Sending results back

You do not need to write anything neat. Copy and paste is fine. Paste text
rather than sending pictures; the words on screen are what I need.

If a step does not match what you see on screen, stop and tell me what is
different. Menus move between versions, and a wrong guess wastes more time than
a question.

If any step fails, that is still a result. A failure answers the question just
as well as a success, and I would rather know than guess.
