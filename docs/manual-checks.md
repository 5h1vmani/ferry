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

## Sending results back

You do not need to write anything neat. Copy and paste is fine. Paste text
rather than sending pictures; the words on screen are what I need.

If a step does not match what you see on screen, stop and tell me what is
different. Menus move between versions, and a wrong guess wastes more time than
a question.

If any step fails, that is still a result. A failure answers the question just
as well as a success, and I would rather know than guess.
