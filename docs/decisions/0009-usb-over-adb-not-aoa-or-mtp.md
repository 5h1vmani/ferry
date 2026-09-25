# 9. USB runs over the adb tunnel. Open Accessory and MTP are not built.

Date: 10 September 2026.
Status: accepted. Replaces the USB rows of the transport table in `PLAN.md`.

## Context

Three ways exist to move bytes between this Mac and this phone over a cable.

| Route | Needs on the phone | Needs on the Mac |
|---|---|---|
| MTP | Nothing. It is built into Android. | An MTP client. macOS has none. |
| adb tunnel | The Ferry app, and USB debugging on | The `adb` binary |
| Android Open Accessory | The Ferry app, in accessory mode | A libusb driver, written here |

The plan rejected MTP with the words "this is what everyone else does badly".
That is not a reason. The author uses Android File Transfer, which is MTP,
every day, and it works. So the rejection needed real grounds or it needed to
go.

## Decision

The adb tunnel is the USB transport. Open Accessory is deferred to the optional
phase. No MTP client is built.

## Why not MTP

MTP cannot write part of a file. `SendObject` sends a whole object or nothing.
So a transfer that dies at 90 percent starts again at zero. Decision record 5,
which resumes a session after a dropped link, cannot hold over MTP.

MTP allows one session at a time. Finder opened five connections during the
order 0 spike. Every one of them would queue behind a single MTP session.

MTP runs only over the cable. Nothing built for it serves the Wi-Fi path. The
whole point of the file operations layer is that one implementation serves
every transport.

MTP has one property nothing else here has. It needs no app on the phone. That
matters for a phone that does not have Ferry installed, which is out of scope.
And for plain USB transfer to a phone without Ferry, OpenMTP already exists,
is free, and is maintained. Ferry building an MTP client would compete with
OpenMTP at what OpenMTP does, instead of doing what nothing does.

## Why not Open Accessory, for now

Its only advantage over the adb tunnel is that the user need not turn on USB
debugging. This phone has USB debugging on, because that is how the app gets
installed. So the advantage serves nobody here.

The cost is three to five weeks: a libusb driver on the Mac, accessory mode on
the phone, and the prompt Android shows every time the cable goes in. That is
the largest single item in the plan, bought for a property nobody needs.

The earlier plan kept it anyway, and gave no user need as the reason. The
real reason to defer it is cost against benefit: three to five weeks for a
property the adb tunnel already provides. The pairing design, the resume
design, and the wire protocol document already record the hard decisions
in this project. A libusb driver would add weeks of code, not a new
decision worth documenting.

## Why adb

It reuses the TCP transport with no changes. `adb forward` opens a local port
on the Mac that reaches a port on the phone. The Noise handshake, the frames,
the file operations, and the resume logic all run over it unchanged. That is
days of work, most of it detecting `adb` and running the forward.

Reconnect after an unplug is the session resume that already exists. The cable
comes back, the tunnel comes back, and the transfer continues from the last
verified chunk.

## Consequences

The phase that existed for Open Accessory disappears. The USB transport lands
in phase 1 beside the network transport.

Ferry on the Mac needs the `adb` binary. Version 1 detects an installed copy,
because anyone installing the Android app already has one. Bundling it is a
later choice, not a blocker.

Open Accessory stays on the optional list, labelled as what it is: deferred
by cost, wanted by no user of this app.

If the scope ever widens to phones without Ferry installed, MTP becomes the
only USB route for them, and this record gets revisited. Until then it is not
built.
