# Information architecture

What the app is made of, how a person moves through it, and what every screen
shows in every state. Structure is settled here, so a screen can change its
surface later without changing its shape.

This is the second version. The first listed four screens per platform and
mapped each to a job. That mapping broke down: four of the eight jobs in
`docs/jobs.md` are served best by no screen at all, and two of them had no
surface anywhere. Layers fix that. The per-screen state tables from the first
version are kept, because they are the part that gets implemented.

Every field these screens read is listed in `docs/engine-contract.md`, with what
the engine has and what it must gain. Where a state depends on something not
built yet, it says so here rather than in a comment somewhere.

## The layers

Six layers. Each answers one question. Three of them appear on every screen
and own none.

| Layer | The question it answers | Jobs | Owns a screen |
|---|---|---|---|
| L0 Presence | What is reachable, over which transport, how fast, and whether this device can be found at all. | 2, 5 | No. Ambient. |
| L1 Trust | Which devices this one will talk to, and how that list changes. | 4 | Yes. Episodic. |
| L2 Access | The other device's files, in place, with no copy step. | 1, 8 | Partly. Finder's and the Files app's, plus the Mac's own Files section. |
| L3 Movement | What is moving, what moved, what stalled and how far it got. | 3, 7 | Yes. The only live list. |
| L4 Truth | What stopped, why, and what to do. | 6 | No. Attached to the object that failed. |
| L5 Record | What a trusted device actually did, afterwards. | 9 | Yes. The only destination. |

Ferry is symmetric: either device serves files to the other, over the same
file operations layer. That was true in the architecture before it was true
in the screens, and L5 is where it becomes visible on both sides.

Three rules follow, and they decide most arguments about where something
goes.

**L0 appears wherever a person already is.** Not in one place. On the Mac
that is the menu bar and the sidebar footer; on the phone it is a row under
the top bar and the persistent notification. All of them read one value and
show the same words. A person asking "is Ferry running" never has to open
anything.

**L4 is never a destination.** An error appears on the transfer, the file, or
the chunk that produced it, at the depth it happened. There is no error
screen, no log of failures, and no notification centre.

**L5 is the only destination.** Every other screen is glanced at or passed
through. The access log is the one thing a person deliberately goes looking
for, which is why it is a screen and presence is not — and why nothing in it
notifies, badges, or interrupts.

### Depth, the second axis

    Device  >  Batch  >  File  >  Chunk

A layer is one line by default and expands downward on demand. The error
table already speaks at chunk depth — "Chunk 14 of IMG_0410.jpg failed to
verify" — so the two lower levels exist in the engine's vocabulary and need a
home in the UI. They get a disclosure, not a screen.

The access log stops at file depth on purpose. One entry per file per
session, never per chunk: a 4.8 GB pull is thousands of reads, and a record
of them is a debug log, not something a person reads.

## The objects

| Object | What it is | Where it lives | Lifetime |
|---|---|---|---|
| Device | A phone or Mac this one has paired with. Name, public key, when paired, last seen. | `peers.rs`, on disk | Until forgotten |
| Root | One folder this device serves, under a name the peer sees: "Desktop", "Downloads". | `Config`, on disk | Until removed |
| Transport | One way to reach a device. USB or Wi-Fi. Available, connected, or not. Speed when moving bytes. | In memory | While the app runs |
| Connection | One encrypted session to one device over one transport. | In memory | Until the link drops |
| Transfer | One file moving in one direction. Progress, state, and any error. | `session.rs`, on disk | Until done or discarded |
| Batch | Several transfers started by one action, shown as one row. "DCIM/Camera, 120 files". Carries why it exists: a person asked, or Ferry decided. | Not yet built | Until every transfer in it ends |
| Entry | A file or folder inside the other device's roots, seen through `list` and `stat`. | In memory | While browsing |
| AccessEntry | One file operation, served or performed: who, which verb, which path, how much, when. | Not yet built | Thirty days |
| AutoCopy | Whether Ferry copies new photos from one device on its own, and what it last did. | Not yet built | Until turned off |
| Pairing | The act of trusting a new device. By code, or by scan. | In memory | Under a minute |
| Reachability | Whether this device advertises and accepts connections. One boolean, one consequence. | In memory | While the app runs |

A person sees Devices, Roots, Batches, Transfers, AccessEntries, AutoCopy,
Pairing, and Reachability directly. Transport is a badge on a device, never
its own screen. Connection is never shown: it is a detail of how a transport
is currently used. Entry appears in the Mac's Files section, and after phase
2 in Finder and the Files app.

Root, Batch, AccessEntry, AutoCopy, and Reachability are new in this version.
Every one of them was already implied by copy in the first version — "3 of
120 files" is a batch, the Reachable switch is reachability — without being
named as an object.

## Navigation

### On the Mac

One window. A 232px sidebar lists devices; the detail pane shows the selected
one. A menu bar item carries presence when no window is open.

```text
Menu bar item                                   L0
├── one row per device, with a TransportBadge
├── the reachability switch
└── "Open Ferry"

Window
├── Sidebar: Devices                            L0 as a list
│   ├── one row per paired device, with a TransportBadge
│   ├── the reachability switch and its consequence
│   └── "Pair a phone"
├── Detail, for the selected device
│   ├── Access: the Finder mount, one line      L2
│   ├── Files: this device's view of the peer    L2
│   ├── Automatic: copy new photos, one switch  L3
│   ├── Transfers: newest first                 L3
│   │   └── a chunk disclosure, where a chunk fact exists
│   ├── Access log: what this pair did           L5
│   └── Info: a footer of four facts, and "Forget this phone"   L1
├── Pairing: a sheet over the window            L1
│   ├── by scan: show a code, then confirm once
│   └── by code: wait, pick, compare six digits
└── Settings: the standard Settings window, Command comma
    ├── Shared folders: Desktop, Downloads
    └── Where pulled files land
```

Info was an equal section in the first version. It is four facts read twice a
year, stacked against things that change every second, so it becomes a
footer. Access, Automatic, and Access log are new.

Six sections in one detail pane is the most this shape will carry. If a
seventh is ever proposed, the pane becomes a list of destinations instead,
and that is a bigger change than adding a section — so the seventh has to
earn it.

### On the phone

One activity. Devices is the home screen. There is no device detail screen: a
phone holds one Mac, so a tap that selects it changes nothing. Transfers sit
under the device row, and Info folds into Settings.

```text
Devices (home)
├── top bar: "Send files" while a Mac is paired, and Settings **new**
├── the reachability row and its consequence    L0
├── one row per paired Mac, with a TransportBadge
├── Transfers, under the row                    L3
├── → Pairing: full screen                      L1
│   ├── by scan: the camera
│   └── by code: the six digits and the short code
└── → Settings                                  L1 + permissions
    ├── This phone: name, shared storage
    ├── Permissions: all files access, notifications
    ├── → Access log                            L5
    └── Paired: the Mac, its fingerprint, Forget this Mac

Persistent notification                         L0
└── the state, and one action to change it

Transfer notifications                          L0  **new**
└── one per batch, or per transfer outside a batch: a progress bar while
    it runs, then Done, or Failed with Retry
```

If a second Mac is ever paired, the device row becomes a destination and this
decision reverses. Nothing else in the structure changes.

A share from another app, `ACTION_SEND` or `ACTION_SEND_MULTIPLE`, and the
"Send files" control both call `push_files` into the Mac's landing folder,
`docs/engine-contract.md` item 5. **new** With one Mac paired there is no
device step. With no Mac paired, the share shows this screen in its empty
state instead.

## Every screen, every state

Every string here follows `docs/voice.md`. These are the real words, not
placeholders. Where a string is new in this version it is marked **new**, and
every new string is written out in `macos/Ferry/Strings.swift`.

### Presence, both platforms

| State | Shows |
|---|---|
| Advertising | The reachability control, on. The wifi icon and "Advertising". No second line. |
| Not advertising | The control, off. The not_reachable icon and "Not advertising". Second line: "This phone cannot be found on Wi-Fi. USB still works." **new** On the Mac: "This Mac cannot be found on Wi-Fi. USB still works." **new** |
| Advertising, quiet on this network | New in item 18. The control stays on. Second line, on the Mac: "Ferry is quiet on this network." **new** when the network is known, or "Ferry cannot read the network name." **new** when it is not. The phone's own row and its notification always show the first line; both phone lines show only in Settings, Networks. |
| Moving, in the menu bar | The transport icon and the speed, in mono: "38 MB/s". |
| Notification, phone, advertising | "Pixel 3 XL is reachable over Wi-Fi." One action: "Stop advertising". **new** |
| Notification, phone, not advertising | "Not advertising. Pixel 3 XL cannot be found on Wi-Fi." **new** One action: "Start advertising". **new** |

The off state moves to `surface_raised` with a `border_strong` edge. It is
never red: nothing dangerous happened. The consequence line is stated because
switching this off silently breaks Wi-Fi transfers, and a person who cannot
see why has no way to guess.

### Devices

| State | Shows |
|---|---|
| Empty | "No phone paired." on the Mac, "No Mac paired." on the phone. A "Pair a phone" or "Pair" control. This is the first run. The presence control is still shown: it is the one true fact about a device that has paired with nothing. A share arriving with no Mac paired lands here too. **new** |
| Populated | One row per device: name, TransportBadge. A device that is moving bytes shows its speed in the badge. On the phone, the top bar also gains "Send files", which opens the system document picker and pushes the chosen files to the paired Mac. **new** |
| Two transports | The active transport in the badge; the spare stated once beside it: "USB also available". **new** A pulled cable is then not a surprise. |
| Not reachable | The device icon in `text_secondary`, the name, a badge reading "Not reachable", and a caption: "Last seen 2 hours ago". Still listed. |

There is no loading state. The list is local and instant.

### Pairing, the Mac, by scan

The default. Three steps instead of five, and no digits to compare.

| State | Shows |
|---|---|
| Offering | The code as a square, large. "Scan this with the phone." **new** Below: "The code expires in 1:48." **new** Two controls: "Use a pairing code instead" **new** and Cancel. |
| Requested | The phone icon in `accent`. "Pixel 3 XL wants to pair." **new** Below: "It scanned this Mac's code over USB. Pairing lets it read and write the shared folders." **new** Two controls: "Pair" and "Refuse". **new** |
| Confirmed | The paired icon in `accent` for one second. The sheet closes and the new device is selected. |
| Expired | An ErrorBlock. "Pairing stopped. The code expired. Show a new code on the Mac." A control: "Show a new code". **new** |

Why no digits here. Comparing six digits proves the same person holds both
devices. Scanning the Mac's screen proves it too, and earlier: the phone
learns the Mac's static key out of band, so there is no window in which a
wrong confirm accepts a stranger. What remains is one named question on
each side, with two answers. The Mac asks whether the phone it sees is
yours. The phone asks the same about the Mac, so a scan of a stranger's
code shows the stranger's name before anything is trusted.

### Pairing, the Mac, by code

Kept, unchanged, for a phone that cannot scan. Reached from "Use a pairing
code instead", and it is the whole of the first version's pairing flow.

| State | Shows |
|---|---|
| Waiting | "Looking for a phone." Below it: "Plug in a cable, or open Ferry on the phone and turn on pairing." A Cancel control. |
| Found | A list of candidates. Each says its transport and its short code: "Phone over USB" or "Phone on Wi-Fi · 3F9A". The person picks one. |
| Code | The six digits, large, grouped three and three. "Confirm this matches on the phone." Confirm and Cancel. |
| Confirmed | As above. |
| Failed | An ErrorBlock. "Pairing stopped. The codes did not match. Try again." Or: "Pairing stopped. The phone left the network. Reconnect and try again." A Retry control. |

### Pairing, the phone

| State | Shows |
|---|---|
| Choosing | Two controls: "Scan the Mac's code" **new** and "Use a pairing code instead". **new** Scanning is first because it is fewer steps when both devices are in reach, which is when pairing happens. |
| Scanning | The camera, a 200pt frame, and one line: "Point the camera at the Mac's screen." **new** One control: "Use a pairing code instead". |
| Camera refused | An ErrorBlock. "Pairing stopped. Ferry cannot use the camera. Grant camera access in Settings, or use a pairing code." **new** |
| Scanned | The name the Mac sent, then "Is this your Mac?" **new** Two controls: Confirm and Cancel. The scan proved a key, not a name. |
| Waiting, code method | "Pairing on. Open Ferry on the Mac." and the four character short code, under a line naming it: "This phone appears on the Mac as". **new** |
| Code | The six digits. "Confirm this matches on the Mac." Confirm and Cancel. Below: "Pairing stops in 1:12." **new** |
| Confirmed | The paired icon in `accent` for one second, then Devices. |
| Timed out | An ErrorBlock. "Pairing stopped. Two minutes passed. Turn pairing on again." **new** |

Camera permission is asked when a person taps to scan, not at first run, so
first run still asks for two things. Refused, the flow falls back to the code
rather than stopping.

The short code is the last four characters of the phone's random mDNS name,
and it is only ever shown in the code method. A person in a room with three
phones can tell which is theirs. A wrong pick is safe anyway, because the six
digit code will not match.

### Access, Mac only

| State | Shows |
|---|---|
| Mount ready | The folder icon in `accent`, "Finder mount ready at", the path in mono, and "Open in Finder". |
| Not ready | The section is absent. Ferry does not state a negative about a thing it has not yet done. |

The phone has no Access section. Until DocumentsProvider ships there is
nothing true to say, and after it ships the Files app says it.

### Files, Mac only

The Mac browses the phone's roots directly, over `list` and `stat`. This was
built before this version of the IA was written, and it stays.

| State | Shows |
|---|---|
| Reading | The platform progress view. "Reading the folder." |
| At the roots | One row per root the peer serves, by its name. "Go up" is disabled and the path reads "/". |
| Populated | Folders first, then files by name. A folder is a control that enters it. A file states its size and offers "Copy to Mac". |
| Empty | "This folder is empty." |
| Failed | An ErrorBlock and a Retry control. |

This is the one view in Ferry with a loading state, and that is honest: a
folder listing is a round trip to another device, unlike the Devices list,
which is local and instant.

It is also the only way to fetch one named file until the Finder mount ships,
and `pull` is the only direction the core has. When the mount lands, this
section becomes a duplicate of Finder and should be reconsidered — not
before.

### Automatic, Mac only

Job 7, which had no screen until now.

| State | Shows |
|---|---|
| Off | "Copy new photos from Pixel 3 XL" **new** and the switch, off. Below: "From DCIM. To Downloads/Ferry. Never deleted, never written back." **new** |
| On, never run | As above, and the switch on. No third line: there is nothing to state yet. |
| On, has run | And a third line: "Last copied 43 files, 2 hours ago." **new** |
| Running | The batch appears in Transfers with "Automatic" where the direction sits. This section does not duplicate its progress. |

One switch, per device, not a global preference: a work phone and a personal
phone are different answers, and the device's own screen is where that answer
belongs.

The rule is stated rather than implied. "Never deleted, never written back"
is the sentence that separates job 7 from the two-way sync Ferry refuses, and
it is the one place a person could reasonably fear the wrong thing.

Last run is a count and a time. Not "up to date", which is an adjective
standing in for a number, and not a tick.

### Transfers

| State | Shows |
|---|---|
| Empty | "No transfers." |
| Active | The batch name and where it came from: "DCIM/Camera, 120 files", then "Automatic · Phone to Mac" **new** or "Phone to Mac". A ProgressLine, "43 of 120 files · 2.1 GB remaining · 38 MB/s", and the TransportBadge. |
| Queued | The batch name, and "Queued." No bar. |
| Paused | The bar stops in `border_strong`. "Paused. The cable was disconnected. Reconnect to continue." No retry control: it resumes on its own. |
| Done | The name, its size, and how long it took: "12 files · 4.8 GB · 3 min". |
| Failed | An ErrorBlock in the row, three parts, and a Retry control. Below it, the chunk disclosure. |

"Automatic" is a word, not a badge, and it sits where the direction sits in
the same type, because it is the same kind of fact: how this transfer came to
exist.

### The chunk disclosure

| State | Shows |
|---|---|
| Collapsed | One line: "Show chunks" **new** and a count in mono: "13 of 96 verified". **new** |
| Expanded | One row per failed or unverified chunk: its index and its state. Verified chunks are summarised, not listed. |
| No chunk facts | Absent. It appears only where the engine holds a chunk-level fact. |

This is a disclosure on ProgressLine, not a screen and not a component.

### The access log

L5. One list per paired device, holding what this device served to that peer
and what it read from it. Job 9.

| State | Shows |
|---|---|
| Empty | "No access yet." **new** |
| Populated | Newest first, grouped by day: "Today", "Yesterday", then the date. One row per entry. |
| One row | The time in mono, then a sentence, then the amount in mono. "14:31 · Pixel 3 XL read Desktop/Q3 notes.md · 48 KB" **new** |
| A list operation | "Pixel 3 XL listed Desktop" and the count: "31 entries". **new** |
| This device's own reads | "This Mac read DCIM/Camera, 120 files" and "4.8 GB". **new** |
| Footer | "Kept for 30 days." **new** |

The subject is always named: "Pixel 3 XL read" or "This Mac read". Never
"you", and never a direction icon — a log is read months later, out of
context, and an arrow does not survive that.

The verbs are the file operations layer's own: read, wrote, listed, deleted,
renamed. A log line and a protocol trace say the same word.

Nothing in this screen notifies, badges, or judges. It records what happened
and does not decide that something was wrong. See `docs/jobs.md`, what
job 9 is not.

### Settings, the Mac

| Group | Setting | Shows |
|---|---|---|
| Shared folders | The roots | One row per root: the folder icon, its name, its path in mono, and "Stop sharing". **new** Below the group title: "Paired phones can read and write these folders. Nothing outside them is served." **new** |
| Shared folders | Add | "Add a folder" **new** and the standard open panel. |
| Where pulled files land | The download folder | The path in mono, and "Choose". |
| Networks | Current network | The name, and "Trust this network" **new** when it is known and not yet trusted. |
| Networks | Trusted list | One row per trusted name, each with "Remove". **new** |

Three settings, where the first version had one. Networks came with item
18: the Wi-Fi networks this Mac trusts. Forgetting a device lives in that
device's Info, because it is about one device. Reachability is not here;
it is presence, and presence is pinned.

Desktop and Downloads are the defaults. They are names a person recognises,
where the first version's single "Ferry" folder was a name Ferry made up.

### Settings, the phone

| Group | Setting | Shows |
|---|---|---|
| This phone | Name | "Pixel 3 XL". The name sent in `hello`. Editable, at most 64 bytes. |
| This phone | Shared storage | "Internal storage". The engine serves one root on the phone. Not editable in phase 1. |
| Permissions | All files access | "Granted" or "Not granted", with a control that opens the system screen. |
| Permissions | Notifications | On Android 13 and later: "Allowed" or "Not allowed", with a control to the system screen. |
| Permissions | Location | New in item 18. "Granted" or "Not granted", with a control that opens the system screen. **new** |
| Networks | Current network | New in item 18. The name, and "Trust this network" **new** when it is known and not yet trusted. |
| Networks | Trusted list | One row per trusted name, each with "Remove". **new** |
| Networks | Quiet reason | Shown only while advertising and quiet on this network. Known: "Ferry is quiet on this network." **new** Unknown: "Ferry cannot read the network name." **new** and an "Open settings" **new** control. |
| Record | Access log | A destination. **new** |
| Paired | The Mac | Its name, its key fingerprint in mono, and "Forget this Mac". |

Reachability has left this screen. It was a switch filed under preferences;
it is a mode with a consequence, reached in a hotel lobby in a hurry, and it
now lives where presence lives.

## First run

### Mac

1. Launch. Devices is empty. "No phone paired. Pair a phone." The presence
   control reads "Advertising".
2. Pair a phone. The sheet opens Offering: the square code and its expiry.
3. The phone scans. Requested: "Pixel 3 XL wants to pair."
4. Pair. Confirmed. Devices shows the phone, reachable, with its transport.

Four steps, where the first version had five. The code flow adds two back
and is one control away.

### Phone

1. Launch. One screen: "Ferry needs two things." All files access, so it can
   serve the shared folders. Notifications, so it can stay reachable. Two
   controls: "Grant access" and "Skip". **new**
2. The system screen for all files access.
3. On Android 13 and later, the notification permission prompt.
4. Devices is empty. "No Mac paired." A Pair control.
5. Pairing. Two controls: scan, or use a code. Scanning asks for the camera
   here, once.
6. The camera. Scan the Mac's screen.
7. "MacBook Pro found. Confirm on the Mac." Then Devices shows the Mac.

Seven steps, two of them Android's own screens, and one of them a camera
prompt that only appears if a person chose to scan.

The first version said the one control on step 1 was "Continue". Rule 9 of
`docs/voice.md` bans that word. The control is "Grant access", which is what
it does. Skip exists because all files access can be refused and this file
already says Devices must still work when it is: a screen with no way past it
contradicts that. Skipping lands on Devices, with "Not granted" in Settings,
and every transfer then fails with "Ferry cannot read the shared folders.
Grant all files access in Settings." That is the three part rule, and it is
better than refusing to open.

## What this requires from the engine

The first version found one gap, the `hello` exchange, and it is now built.
Writing these states found fifteen more. They are in `docs/engine-contract.md`,
each with its fields and its build order. The four that change this file if
they are answered differently:

1. **Reachability has a setter and no getter.** Every presence surface here
   reads a value that cannot currently be read.
2. **A batch is not an object.** "43 of 120 files" is one row standing for
   many transfers, and `TransferInfo` is one file.
3. **`Config` serves one root.** Desktop and Downloads means several named
   roots, and every path in the protocol then begins with a root name.
4. **The access log has no source.** Logging belongs at the file operations
   layer, not the transfer engine: a Finder browse produces no transfer, so a
   log built on transfers would miss most of what job 9 exists to record.
