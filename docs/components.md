# The component inventory

The custom views that appear on more than one screen. Each is built once per
platform, from native controls and the tokens in `design/`. Nothing here is
built if a native control already does the job.

Six components. If a seventh appears, it must earn its place by being used on
two screens.

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

## DeviceRow

One paired device in the Devices list.

| State | Shows |
|---|---|
| Reachable | device icon, name, TransportBadge |
| Not reachable | device icon in `text_secondary`, name, TransportBadge, and a `caption` line: "Last seen 2 hours ago" |
| Selected, Mac only | the platform's sidebar selection |

Screen reader says: the name, then the badge text, then last seen if present.

Built from: the platform list row. On the Mac, a `NavigationLink` in a sidebar
`List`. On the phone, a Material `ListItem`.

## ProgressLine

One transfer's progress, in one line.

| State | Shows |
|---|---|
| Active | a determinate bar in `accent`, then "3 of 120 files · 2.1 GB remaining · 38 MB/s" in `mono` |
| Paused | the bar stops and turns `border_strong`, then "Paused" and the reason, from the error table |
| Done | no bar, the done icon, "120 files · 4.8 GB · 3 min" |
| Failed | no bar, replaced by an ErrorBlock |

Screen reader says the text line, and for active, "Transferring, 2 percent"
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
come from the error table, never from the view.

## PairingCode

The six digits, on both devices at once.

| State | Shows |
|---|---|
| Showing | "481 920" in `display` type with monospaced digits, grouped three and three. Below, in `body`, the instruction names the other device: "Confirm this matches on the phone." on the Mac, "Confirm this matches on the Mac." on the phone. Confirm and Cancel controls. |
| Confirmed | the paired icon in `accent` for one second, then the view closes |
| Mismatched | an ErrorBlock takes its place |

Screen reader says: "Pairing code, four eight one, nine two zero" as digits,
then the instruction, then the controls. Digits are read one at a time so they
can be compared against the other screen.

Built from: one text view and two buttons. The gap between the two groups is
`space.3`. The code is never in a text field. It is compared by eye, not typed.

## EmptyState

One line and one action, centred. Used by Devices with no devices and by
Transfers with no transfers.

| Where | Line | Action |
|---|---|---|
| Devices, Mac | "No phone paired." | Pair a phone |
| Devices, phone | "No Mac paired." | Pair |
| Transfers | "No transfers." | none |

`body` type in `text_secondary`. The action is the platform's primary button.

Screen reader says the line, then the action if present.

## What is deliberately not a component

A toolbar, a tab bar, a navigation bar, a sheet, a list, a button, a text
field, a switch, a progress view. All of these are the platform's own. Using
them unchanged is what makes the app look like it belongs on the device.
