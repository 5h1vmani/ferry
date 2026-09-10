# The design system

This is the whole of it. A two-screen tool does not need more, and more would
fight the platforms it runs on.

## Platforms first

The Mac app follows Apple's Human Interface Guidelines and uses native SwiftUI
controls. The phone app follows Material 3 and uses native Compose controls.

No custom widget is built unless a job in `docs/jobs.md` cannot be served by a
native one. That has not happened yet.

## One accent colour

The accent is `#3368A0`. It is a muted mid blue. It reads as a tool, not as a
brand.

A full twelve-step scale, in light and dark, is generated from that one value
using the Radix Colors custom palette method. The scale lives once, in
`design/colors.json`, and both apps consume generated code from it. Nobody
types a hex value into a view.

Neutrals are the platform's own greys. Contrast is guaranteed by the scale, at
4.5 to 1 or better for text.

## Type, icons, motion

System fonts only. SF on the Mac, Roboto on the phone. Both respect the
person's chosen text size.

SF Symbols on the Mac, Material Symbols on the phone. One table maps each
semantic name, such as `device`, `paired`, `transfer`, `usb`, `wifi`, to its
symbol on each platform. The table lives once.

Motion is the platform default. Nothing custom in version 1.

## Words

Every string follows `docs/voice.md`. Strings live in one file per app. A
string inline in a view is a bug.

## Accessibility is not a later step

Every control carries a label a screen reader can speak. Every screen is
usable from the keyboard on the Mac. Every screen is checked with VoiceOver
and TalkBack before it is called done. This costs minutes now and weeks later.

## The screens, from the jobs

Phase 1 has four screens on each platform, and no more.

| Screen | Serves | Shows |
|---|---|---|
| Devices | Jobs 1, 2, 4 | Paired devices, which is reachable, over which transport, at what speed |
| Pairing | Jobs 4, 5 | The six digit code, and a confirm control on both devices |
| Transfer | Jobs 2, 3, 6 | Files done of total, bytes remaining, speed, transport, and any error in three parts |
| Settings | Jobs 5, 4 | Advertising on or off, the shared folder, and forget this device |

The active transport and its speed are visible on every screen that moves
bytes. That is the architecture, made visible.
