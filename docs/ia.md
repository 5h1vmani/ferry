# Information architecture

What the app is made of, how a person moves through it, and what every screen
shows in every state. Structure is settled here, so a screen can change its
surface later without changing its shape.

## The objects

| Object | What it is | Where it lives | Lifetime |
|---|---|---|---|
| Device | A phone or Mac this one has paired with. Name, public key, when paired, last seen. | `peers.rs`, on disk | Until forgotten |
| Transport | One way to reach a device. USB or Wi-Fi. Available, connected, or not. Speed when moving bytes. | In memory | While the app runs |
| Connection | One encrypted session to one device over one transport. | In memory | Until the link drops |
| Transfer | One file moving in one direction. Progress, state, and any error. | `session.rs`, on disk | Until done or discarded |
| Entry | A file or folder inside the other device's shared root, seen through `list` and `stat`. | In memory | While browsing |
| Pairing | The act of trusting a new device. The code and the confirm. | In memory | Under a minute |

A person sees Devices, Transfers, and Pairing directly. Transport is shown as
a badge on a device, never as its own screen. Connection is never shown at
all; it is a detail of how a transport is currently used. Entry appears in
phase 2, through Finder.

## Navigation

Four screens on each platform. They compose differently, because the two
platforms have different native shapes.

### On the Mac

One window. A sidebar on the left lists Devices. The area on the right shows
the selected device. This is the shape of Mail, Notes, and Finder, and it is
what a Mac tool looks like.

```text
Window
├── Sidebar: Devices
│   ├── one row per paired device, with a TransportBadge
│   └── "Pair a phone" at the bottom
├── Detail, for the selected device
│   ├── Transfers: every transfer for this device, newest first
│   └── Info: name, when paired, key fingerprint, "Forget this phone"
├── Pairing: a sheet over the window
└── Settings: the standard Settings window, Command comma
```

No tabs, no toolbar clutter. Transfers and Info are two sections of one detail
view, stacked, because a device rarely has more than a handful of transfers.

### On the phone

One activity. Devices is the home screen. Tapping a device opens it. Pairing
is a full-screen flow. Settings is reached from the top bar.

```text
Devices (home)
├── one row per paired Mac, with a TransportBadge
├── a "Pair" button
├── → Device: Transfers, then Info with "Forget this Mac"
├── → Pairing: full screen
└── → Settings: from the top bar
```

While the phone is reachable, a persistent notification says so, with one
action to stop. That notification is the foreground service Android needs, and
it is also the honest answer to "is Ferry running".

## Every screen, every state

Every string here follows `docs/voice.md`. These are the real words, not
placeholders.

### Devices

| State | Shows |
|---|---|
| Empty | "No phone paired." A "Pair a phone" control. This is the first run. |
| Populated | One row per device: name, TransportBadge. A device that is moving bytes shows speed in the badge. A device that is not reachable shows "Not reachable" in the badge, greyed, still listed. |

There is no loading state. The list is local and instant.

### Pairing

This table is the Mac's screen. The phone's pairing states are the ones in
the first run section below, because the phone waits and shows a code rather
than searching and picking.

| State | Shows |
|---|---|
| Waiting | "Looking for a phone." Below it: "Plug in a cable, or open Ferry on the phone and turn on pairing." A Cancel control. |
| Found | A list of candidates. Each says its transport and its short code: "Phone over USB" or "Phone on Wi-Fi · 3F9A". The person picks one. |
| Code | The six digits, large. "Confirm this matches on the phone." Confirm and Cancel controls. |
| Confirmed | The sheet closes. The new device is selected in Devices. |
| Failed | An ErrorBlock. "Pairing stopped. The codes did not match. Try again." Or: "Pairing stopped. The phone left the network. Reconnect and try again." A Retry control. |

The short code in the Found state is the last four characters of the phone's
random mDNS name. The phone shows the same four characters on its own pairing
screen, so a person in a room with three phones can tell which is theirs. A
wrong pick is safe anyway, because the six digit code will not match.

### Transfers

| State | Shows |
|---|---|
| Empty | "No transfers." |
| Active | One row per transfer: file name, a ProgressLine, "3 of 120 files · 2.1 GB remaining · 38 MB/s", and the TransportBadge. |
| Paused | The row's ProgressLine stops. "Paused. The cable was disconnected. Reconnect to continue." It resumes on its own when the device is reachable again. |
| Done | The row shows the file name, its size, and how long it took. |
| Failed | An ErrorBlock in the row, three parts, and a Retry control. |

### Settings

The Mac has one setting in phase 1: the shared folder. Forgetting a device
lives in that device's Info, not here, because it is about one device.

The phone has more, because Android needs more from a person.

| Setting | Shows |
|---|---|
| Reachable | A switch. On: the phone advertises and accepts connections. Off: neither. |
| Shared folders | The fixed list: DCIM, Pictures, Movies, Music, Download, Documents. Not editable in phase 1. |
| All files access | "Granted" or "Not granted", with a control that opens the system screen. Ferry cannot serve files without it. |
| Notifications | On Android 13 and later: "Allowed" or "Not allowed", with a control to the system screen. Needed for the reachable notification. |

## First run

### Mac

1. Launch. Devices is empty. "No phone paired. Pair a phone."
2. Pair a phone. Pairing opens in the Waiting state.
3. The phone appears. Found. The person picks it.
4. Code. The person checks both screens and confirms.
5. Confirmed. Devices shows the phone, reachable, with its transport.

Five steps. Under a minute. Nothing else is asked.

### Phone

1. Launch. One screen explains what Ferry needs and why: all files access, so
   it can serve the shared folders; a notification, so it can stay reachable.
   One control: Continue.
2. The system screen for all files access. The person grants it and comes
   back.
3. On Android 13 and later, the notification permission prompt.
4. Devices is empty. "No Mac paired." A Pair control.
5. Pairing. The phone shows "Pairing on. Open Ferry on the Mac." and its four
   character short code. Pairing mode times out after two minutes.
6. Code. The person checks both screens and confirms.
7. Confirmed. Devices shows the Mac.

Seven steps, two of them Android's own screens. If all files access is not
granted, Devices still works, but every transfer fails with "Ferry cannot read
the shared folders. Grant all files access in Settings." That is the three
part rule, and it is better than refusing to open.

## What this requires from the protocol

Writing the states exposed one gap. Devices shows a name. Where does it come
from?

The mDNS name is random by design. The adb serial is a number. Neither is a
name. So a device has to say its name inside the encrypted channel, after the
handshake, before anything else.

That is a `hello` exchange: each side sends its display name, at most 64
bytes, as the first frame after `XX` or `KK` completes. The protocol does not
have it yet. It is phase 1 work, and it is in `PLAN.md`.

The name is chosen by the person and defaults to the device model. It is
shown, never trusted for anything. Identity is the key.
