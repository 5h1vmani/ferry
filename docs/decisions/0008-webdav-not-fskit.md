# 8. The Finder mount uses WebDAV, not FSKit

Date: 10 September 2026.
Status: accepted.

## Context

Three routes can put a phone's files in Finder on a Mac.

1. A WebDAV server the Mac mounts. Proven to work in the order 0 spike, with no
   entitlement and no payment. See `docs/spike-0-findings.md`.
2. A File Provider extension. Better byte range handling, needs an Apple
   entitlement, and the entitlement question is still open.
3. FSKit, which makes a real filesystem in user space. It would be the cleanest
   of the three, with no HTTP server, no locking, and no local port to protect.

The plan said FSKit was broken on macOS 26, citing macOS 26.1 and 26.2. This
machine runs 26.6.2, so that claim was four point releases old. It was worth
rechecking before ruling FSKit out.

## Decision

Build the Finder mount on WebDAV. Do not spend time on FSKit now.

## Reasons

The claim was stale in its version numbers and correct in substance.

Three separate FSKit faults are on public record, not one.

- `fskitd` refuses unprivileged clients with an ExtensionKit error. This was
  reproduced against Apple's own FSKitSample, not only third-party code.
  Reported on macOS 26.1 and 26.2.
  <https://github.com/andrewgazelka/loaf/issues/1>, December 2025.
- Mounting fails intermittently. It works once after a reboot, then fails on
  the next remount. Many people report it independently, across macOS 26.1,
  26.3, 26.3.1, 26.4, 26.4.1, and 26.5.1. The most recent comment is dated
  22 June 2026. <https://github.com/macfuse/macfuse/issues/1132>, still open.
- `fskitd` threads deadlock and `mount` never returns. Only a reboot clears it.
  An Apple engineer reproduced it and filed an internal bug.
  <https://developer.apple.com/forums/thread/819160>, March 2026.

No release note, forum post, or issue comment confirms a fix in macOS 26.6.
That is an absence of evidence rather than evidence of absence, so the honest
statement is that nobody has reported it working.

FSKit does work for one shape of problem. ExtendFS ships on the Mac App Store
and mounts ext4 drives read only.
<https://github.com/kthchew/ExtendFS>, release 1.2.1 dated 31 August 2026.

That case is a real block device, read only. A phone reached over Wi-Fi or a
cable is neither. The macFUSE FSKit backend, which is the closest published
thing to what Ferry needs, is the one hitting the intermittent mount fault.

Recommendation, and a judgment rather than a cited fact: Ferry's case sits with
the failing examples, not the working one. Betting an afternoon on it is fair.
Betting a phase on it is not.

## Consequences

Phase 2 ships the WebDAV bridge, which is already proven on this machine.

The manual check that asked a person to test FSKit in Xcode is removed. The
question is answered well enough without it.

Revisit FSKit if someone publishes a working network-backed FSKit filesystem,
or if Apple confirms a fix. The technology is right for this job. It is the
current state of it that is not.

## A second reason, found later

A free Apple personal team cannot use FSKit at all.

Apple's own capability table lists **FSKit Module** with no mark in the free
"Apple Developer" column. It is available only to the paid Apple Developer
Program and to Developer ID.
<https://developer.apple.com/help/account/reference/supported-capabilities-macos>,
read 10 September 2026.

So FSKit costs 99 US dollars a year before it can be tried at all. That was not
known when this record was written, and it makes the decision easier rather
than harder.
