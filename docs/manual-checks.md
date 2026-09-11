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
4. In Terminal, in the `ferry` folder, run:

```bash
source scripts/env.sh && cd android && gradle assembleDebug -q && adb install -r app/build/outputs/apk/debug/app-debug.apk
```

   The last line should say `Success`. If it says `no devices`, unplug and
   plug the cable again and check step 3.

### Part B: build and open the Mac app

In Terminal, in the `ferry` folder, run:

```bash
cd macos && xcodegen generate -q && xcodebuild -project Ferry.xcodeproj -scheme Ferry -configuration Debug -derivedDataPath build -quiet build && open build/Build/Products/Debug/Ferry.app
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

1. On the Mac, select the phone. The detail shows the phone's storage. Open
   DCIM, then Camera.
2. Pick a photo and click Copy to Mac.
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

1. On the Mac, open the phone, go into `DCIM`, and click Copy to Mac on the
   `Camera` folder.
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

### Part G: pairing by scan, from the phone

Added 11 September 2026. This is `docs/engine-contract.md` item 12, the
phone's half.

1. On the phone, forget the Mac from Settings, so pairing can run again.
2. On the phone, tap Pair, then "Scan the Mac's code". On the Mac, click
   "Pair a phone"; it should show the square code.
3. Grant the camera the first time the phone asks for it. The phone
   should show "Point the camera at the Mac's screen."
4. Point the camera at the Mac's code. The phone should show the Mac's
   name and "Is this your Mac?"
5. On the phone, tap Confirm.
6. On the Mac, confirm the phone that scanned in. Devices on the phone
   should then show the Mac, reachable.

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
5. On the phone, wait a few seconds. The Mac should drop out of the
   phone's device list.
6. On the Mac, add the home network to the trusted list again by hand.
   The Mac should become reachable to the phone again within a few
   seconds.
7. On the phone, next time Android asks for the location permission,
   refuse it. Every screen and every transfer already working should
   behave exactly as it did before the refusal.

### What to send back

For each step, one line: worked, or what you saw instead. Paste the exact
words from any error block.

## Sending results back

You do not need to write anything neat. Copy and paste is fine. Paste text
rather than sending pictures; the words on screen are what I need.

If a step does not match what you see on screen, stop and tell me what is
different. Menus move between versions, and a wrong guess wastes more time than
a question.

If any step fails, that is still a result. A failure answers the question just
as well as a success, and I would rather know than guess.
