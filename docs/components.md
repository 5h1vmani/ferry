# The component inventory

The custom views that appear on more than one screen. Each is built once per
platform, from native controls and the tokens in `design/`. Nothing here is
built if a native control already does the job.

**Nine components.** The first version had six. Two were added, each because
one value is now shown in more than one place and must say the same thing
everywhere. Six candidates were rejected; they are at the bottom with the
reason.

A ninth, TransferRow, came with the third run on 11 September 2026. It
earns its place: the Mac's Transfers section and the phone's Transfers
list under the device row both draw the same row, in the same order.

If a tenth appears, it must earn its place the same way.

Every string follows `docs/voice.md`. Every component carries a label a
screen reader can speak, written here so it is not invented twice.

## TransportBadge

Says how a device is reachable right now. Appears on every DeviceRow and on
every active transfer.

| State | Shows | Screen reader says |
|---|---|---|
| USB, idle | usb icon, "USB" | "Connected over USB" |
| USB, moving | usb icon, "USB · 38 MB/s" | "Connected over USB, 38 megabytes per second" |
| Wi-Fi, idle | wifi icon, "Wi-Fi" | "Connected over Wi-Fi" |
| Wi-Fi, moving | wifi icon, "Wi-Fi · 24 MB/s" | "Connected over Wi-Fi, 24 megabytes per second" |
| Connecting | wifi or usb icon, "Connecting" | "Connecting" |
| Not reachable | not_reachable icon, "Not reachable", secondary text colour | "Not reachable" |

Built from: an icon and a label in a row, `label` type, `mono` for the speed.
Colours: `text_secondary` when not reachable, `text` otherwise. No background.

The spare transport is **not** a state of this component. "USB also
available" sits beside the badge as a plain line, because a badge that names
two paths stops answering "which one is carrying this".

## DeviceRow

One paired device in the Devices list.

| State | Shows |
|---|---|
| Reachable | device icon, name, TransportBadge |
| Reachable, spare transport | as above, and a line: "USB also available" |
| Not reachable | device icon in `text_secondary`, name, TransportBadge, and a `caption` line: "Last seen 2 hours ago" |
| Selected, Mac only | the platform's sidebar selection |

Screen reader says: the name, then the badge text, then the spare transport
if present, then last seen if present.

Built from: the platform list row. On the Mac, a `NavigationLink` in a sidebar
`List`, 32px. On the phone, a Material `ListItem`, 72px. On the phone the row
is not a link: there is no device detail screen.

## PresenceControl

New in this version. Whether this device advertises and accepts connections,
and what it costs when it does not. Job 5's only control.

Appears in four places: the Mac's sidebar footer, the Mac's menu bar item, the
phone's home screen under the top bar, and the phone's persistent
notification. All four read one value. That is why it is a component and not
four views.

| State | Shows |
|---|---|
| Advertising | wifi icon, "Advertising", the platform switch, on. No second line. |
| Not advertising | not_reachable icon, "Not advertising", the switch, off, and a second line: "This phone cannot be found on Wi-Fi. USB still works." The block moves to `surface_raised` with a `border_strong` edge. |
| Changing | the switch in its platform's own transitional state. Nothing else moves. |
| In the notification | the state as a sentence, and one action: "Stop advertising" or "Start advertising". No switch; a notification action is a button. |
| In the menu bar, moving | the transport icon and the speed in mono, beside the control. |

Never red. Switching this off is a thing a person chose, not a thing that
went wrong. The consequence line is stated because the failure it causes is
silent and would otherwise have to be guessed.

Screen reader says: "Advertising, on" or "Not advertising, off", then the
consequence line, then "switch".

Built from: the platform switch and two labels. `Toggle` in a `Section`
footer on the Mac; a Material `ListItem` with a trailing `Switch` on the
phone. The switch is never a custom control.

## AccessLogRow

New in this version. One file operation, as a sentence. L5, job 9.

Appears on the Mac's Access log section and the phone's Access log screen.

| State | Shows |
|---|---|
| Served, read | the time in mono, "Pixel 3 XL read Desktop/Q3 notes.md", the amount in mono: "48 KB" |
| Served, written | "Pixel 3 XL wrote Downloads/scan.pdf" |
| Served, listed | "Pixel 3 XL listed Desktop", and a count: "31 entries" |
| Performed by this device | "This Mac read DCIM/Camera, 120 files", and the amount |
| Rolled up | a file count inside the sentence: "DCIM/Camera, 120 files". Never one row per chunk. |

The subject is always named. Never "you", never "your phone", and never a
direction icon: a log is read months later, out of context, and an arrow does
not survive that. The verb is the file operations layer's own word, so a log
line and a protocol trace agree.

The path is `mono`, because it is machine-produced. The sentence around it is
`body`. Nothing in the row is coloured, and nothing in it is a control: a row
states a fact and offers no judgment and no action.

Screen reader says the sentence, then the time, then the amount. In that
order, because the sentence is what a person is looking for and the numbers
qualify it.

Built from: a row of three labels on the Mac; a two-line Material `ListItem`
on the phone, where the sentence is the headline and the time and amount are
the supporting line.

## TransferRow

One group of transfers as one row of the Transfers section. New in the
third run, 11 September 2026.

Appears on the Mac's Transfers section and the phone's Transfers list,
under each device row.

| Part | Shows |
|---|---|
| Label and origin | The batch or file name, then the origin and direction: "Automatic · Phone to Mac", or just the direction. |
| Progress | The states ProgressLine states below: queued, active, paused, done, or failed. |
| Chunk disclosure | Present only where the engine holds a chunk-level fact, today only after a verify failure. See "The chunk disclosure" below. |

Screen reader says: the label, then the origin and direction, then
ProgressLine's own words.

Built from: a label and a caption in a row, then ProgressLine's states,
then the chunk disclosure, in a column. `TransferRow.swift` on the Mac;
`TransferRow.kt` on the phone.

### The chunk disclosure

Part of TransferRow, not a component of its own. It appears under the row
only where the engine holds a chunk-level fact, which today means only
after a verify failure.

| State | Shows |
|---|---|
| Collapsed | "Show chunks" and a count in mono: "13 of 96 verified" |
| Expanded | one row per failed or unverified chunk: index and state. Verified chunks are summarised, never listed. |
| Absent | when there is no chunk fact to show |

Built from: `DisclosureGroup` on the Mac. A clickable row and
`AnimatedVisibility`, in `ChunkDisclosure.kt`, on the phone. Collapsed by
default, always. It is the bottom of the depth axis in `docs/ia.md`, and
it is one interaction away so that it is never in the way.

## ProgressLine

One transfer or one batch's progress, in one line.

| State | Shows |
|---|---|
| Queued | no bar, "Queued." |
| Active | a determinate bar in `accent`, then "43 of 120 files · 2.1 GB remaining · 38 MB/s" in `mono` |
| Paused | the bar stops and turns `border_strong`, then "Paused" and the reason, from the error table |
| Done | no bar, the done icon, "120 files · 4.8 GB · 3 min" |
| Failed | no bar, replaced by an ErrorBlock |

The bar advances linearly. An easing curve would be a small lie about
throughput.

Where a batch came from — "Automatic · Phone to Mac" — is stated by the row
around this component, not by this component. Origin and direction are facts
about the batch, not about its progress.

Screen reader says the text line, and for active, "Transferring, 2 percent",
updated no more than once every 5 seconds so it does not talk over itself.

Built from: the platform progress view and a label. Never a custom drawn bar.

## ErrorBlock

The three-part rule from `docs/voice.md`, as a view. What stopped, why, what
to do. Any part that is unknown is left out, never guessed.

| Part | Type | Colour |
|---|---|---|
| What stopped | `body`, semibold | `text` |
| Why | `body` | `text_secondary` |
| What to do | `body` | `text` |
| Retry, when one makes sense | the platform's secondary button | `accent` |

The failed icon sits at the left. The whole block sits on `surface_raised`
with `radius.medium` and `space.3` padding. Never red. Red is for danger, and
a paused transfer is not dangerous.

Screen reader says the three parts in order, then "Retry, button" if present.

Built from: three text views in a column, the icon, and a button. The words
come from the table generated from `design/errors.json`, never from the view.
The four QR pairing errors in `docs/engine-contract.md` item 12 render through
this component like every other.

## PairingCode

The six digits, on both devices at once. The code method only: a scan shows
no digits.

| State | Shows |
|---|---|
| Showing | "481 920" in `display` type with monospaced digits, grouped three and three. Below, in `body`, the instruction names the other device: "Confirm this matches on the phone." on the Mac, "Confirm this matches on the Mac." on the phone. Confirm and Cancel controls. |
| Showing, phone | as above, and under it the short code with the line that names it: "This phone appears on the Mac as 3F9A", and the remaining time: "Pairing stops in 1:12." |
| Confirmed | the paired icon in `accent` for one second, then the view closes |
| Mismatched | an ErrorBlock takes its place |

Confirmed is shared with the scan method, which is why it stays here rather
than moving into either flow.

Screen reader says: "Pairing code, four eight one, nine two zero" as digits,
then the instruction, then the controls. Digits are read one at a time so
they can be compared against the other screen.

Built from: one text view and two buttons. The gap between the two digit
groups is `space.3`. The code is never in a text field. It is compared by
eye, not typed.

## EmptyState

One line and one action, centred. Used wherever a list has nothing in it.

| Where | Line | Action |
|---|---|---|
| Devices, Mac sidebar | "No phone paired." | Pair a phone |
| Devices, Mac detail | "No phone selected." | none |
| Devices, phone | "No Mac paired." | Pair |
| Transfers | "No transfers." | none |
| Access log | "No access yet." | none |
| Files | "This folder is empty." | none |

`body` type in `text_secondary`. The action is the platform's primary button.

Screen reader says the line, then the action if present.

## Rejected, with the reason

**PairingQR**, the square code and its expiry on the Mac. One screen, one
platform, one state. A view in the pairing sheet, not a component. The code
itself is rendered by the platform — `CIQRCodeGenerator` on the Mac — from
bytes the engine serialises, so there is nothing custom to build and nothing
to draw by hand.

**QrScanner**, the camera preview that reads the Mac's code, on the phone.
One screen, one platform, one state. A view in the pairing flow, not a
component. The camera preview and the barcode reader are both the
platform's own, CameraX and ML Kit, so there is nothing custom to build
and nothing drawn by hand. It is the phone's half of PairingQR above.

**AutomaticSection**, job 7's switch and its three lines. One screen, one
platform. A view. If the phone ever gains an equivalent — copying the Mac's
Desktop on its own, which nothing asks for — it earns the place then.

**SharedRootRow**, one root in the Mac's Settings. One screen. A view. The
phone serves one root and does not choose it.

**AccessLine**, the Finder mount line on the Mac. One screen, one platform,
one state. A view in `DeviceDetail`.

**NetworkRow**, the current network and the trusted list, in the Networks
section of Settings, item 18. One screen on each platform: a view, not a
shared component. `CurrentNetworkRow` and `TrustedNetworkRow` on the Mac,
in `SettingsView.swift`; a row for the current network and one for each
trusted name inside `NetworksSection`, in `SettingsScreen.kt`, on the
phone. Each platform draws its own Settings screen once.

**FileBrowser**, a list of the other device's roots. It exists, on the Mac,
as the Files section of `DeviceDetail` — built before this version of the
inventory was written. It is not a component because it appears on one screen
of one platform. It is also the one view in Ferry with a loading state, which
is honest: a folder listing is a round trip, unlike the Devices list.

It stays because it is the only way to fetch one named file before the Finder
mount ships. It earns a place in this inventory the day the phone gains an
equivalent, and it should be reconsidered the day the mount makes it a
duplicate of Finder. Whether the phone ever needs one is blocked on
`docs/manual-checks.md` task 3: whether the Files app is a usable browser for
a DocumentsProvider-backed root.

## What is deliberately not a component

A toolbar, a tab bar, a navigation bar, a sheet, a list, a button, a text
field, a switch, a progress view, a disclosure group, a menu bar item, a
notification, a camera preview, a QR renderer, an open panel. All of these
are the platform's own. Using them unchanged is what makes the app look like
it belongs on the device.
