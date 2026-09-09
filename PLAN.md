# Ferry: build plan

Date written: 9 September 2026.
Status: order 0 is done, apart from two items that need a person at the
keyboard. See `docs/spike-0-findings.md`. Phase 1 has not started.

## 1. Goal

Build a file transfer app between macOS and Android.

Two purposes, both real:

1. A portfolio project on GitHub that shows systems engineering skill.
2. An app I will personally use on my own Mac and my own phone.

Not a purpose: revenue. Not a purpose: a public product with many users.

These two purposes point at the same features. That is the reason to build it.

## 2. Why this is worth building

Google discontinued Android File Transfer for Mac in May 2024. Google shipped
no replacement. Google shipped Quick Share for Windows but never for macOS.
Apple will not open AirDrop to Android. So no vendor covers this gap.

LocalSend is free, open source, and good. It sends files over the local
network. It works well.

LocalSend does not let you browse the phone inside Finder. Nothing does. That
is the gap this project fills, and it is the headline feature.

The second feature is a USB path. USB is not faster than good Wi-Fi. USB is
more reliable. It works when the network blocks device discovery, when you are
on guest Wi-Fi, and when the router is bad. See section 3.

## 3. The transport ladder

The app picks the best working path, and shows the user which path is active.

Android Open Accessory runs over USB 2.0 bulk endpoints. Its throughput lands
in the same range as good 5 GHz Wi-Fi. So USB is not a speed feature. USB is a
reliability feature. It works on networks where discovery fails.

| Transport | Friction | Decision |
|---|---|---|
| Wi-Fi on the local network, mDNS and TCP | None | Build. Default path. |
| USB with an adb tunnel | User must enable USB debugging once | Build. Developer transport and fallback. Reuses the TCP code. |
| USB with Android Open Accessory | Plug in, accept a prompt on the phone | Build, but see the note below. |
| USB with MTP | Plug in | Skip. This is what everyone else does badly. |
| USB tethering | Turn tethering on | Test first. See open question 4. |
| Phone local-only hotspot | Mac drops its Wi-Fi and loses internet | Defer. Fallback when the LAN blocks discovery. |
| Wi-Fi Direct or AWDL | Not applicable | Cannot build. macOS exposes no public API. |
| Bluetooth Low Energy | None | Cut. mDNS covers the network case. USB covers the cable case. |
| Cloud relay | Needs a code or an account | Out of scope. |

### An honest note on Android Open Accessory

Its only advantage over the adb tunnel is that the user does not need to enable
USB debugging. This phone already has USB debugging enabled, because that is
how the app gets installed. So that advantage serves nobody here.

The adb tunnel already delivers the reliability that decision record 4 chose.
Android Open Accessory is therefore kept for the portfolio purpose, not the
personal one. Writing a libusb driver and handling accessory mode is the
hardest systems work in this project, and that is the reason to do it. The plan
should not pretend otherwise.

If the tethering test in open question 4 passes, reconsider both USB paths.

USB tethering was previously ruled out here on the grounds that Android
defaults to RNDIS, which macOS does not support. That claim may be out of date.
Open question 4 settles it in five minutes.

## 4. Architecture

### Two layers

**The file operations layer.** A small set of remote operations: `list`,
`stat`, `read(path, offset, length)`, `write(path, offset, bytes)`, `mkdir`,
`delete`. Either side can serve it. Either side can call it.

**The transfer engine.** A transfer is a loop of `read` or `write` calls on
top of that layer. Pushing from Mac to phone is repeated `write`. Pulling from
phone to Mac is repeated `read`. Same frames, same chunking, same integrity
checks, same resume logic, written once.

Push and pull are therefore not two features. They are one protocol used in
two directions.

The `offset` and `length` arguments matter. Finder must show files without
downloading them, and fetch bytes on demand. A whole-file read API cannot do
that. Range reads are designed in from the start.

### Security

Noise protocol, not TLS. The `XX` pattern handles first pairing. The `KK`
pattern handles every connection after that, using the pinned static key of
each device.

Pairing needs more than a short code. In `XX` the initiator sends its static
key last, so an attacker in the middle can generate keys until the code
matches. A six digit code falls in seconds. Pairing therefore commits to a
random nonce before revealing it, runs only inside a user-started pairing mode
on the phone, needs confirmation on both screens, and prefers the USB cable.
See decision record 6.

The long-lived private key lives in app-private storage on Android, excluded
from Auto Backup, and in the Keychain on macOS. Android Auto Backup is on by
default, so without the exclusion the private key reaches the user's Google
Drive. macOS Keychain access is tied to the code signature, so a stable signing
certificate is needed during development.

Reason: the pairing model is "pin the peer's static public key", which is
exactly what Noise does. There is no certificate generation and no PKI
vocabulary in the protocol document. Noise also runs over any byte stream, so
the USB pipe and the TCP socket use identical code. TLS only wins if a browser
needs to connect, and no browser needs to connect.

### Integrity

BLAKE3. It is fast, it parallelises, and its tree structure gives per-chunk
verification and a whole-file root hash from a single pass.

### Each side shares one root, and only one

The Mac shares a folder the user chooses.

The phone shares a fixed list of top-level folders: `DCIM`, `Pictures`,
`Movies`, `Music`, `Download`, and `Documents`. The Android app holds broader
access than that. Limiting the shared root limits the damage if the app or its
transport is ever compromised.

Path checks in the core are lexical. They cannot see what the filesystem
resolves. A symlink inside the root that points outside it passes all of them,
and so does a FIFO. The filesystem layer opens paths with `O_NOFOLLOW_ANY` on
macOS, refuses anything that is not a regular file or directory, and confirms
the resolved path still sits inside the root.

### Sessions outlive connections

A transfer gets an identifier that does not belong to any connection. Both
sides persist a manifest to disk: the chunk list, and which chunks are
confirmed. When the Wi-Fi drops or the cable is pulled, a new connection
resumes the same session from the manifest.

The app never migrates a live stream between transports. That is a hard
problem and it buys nothing here. Resuming a session gives the user the same
result for a small fraction of the work.

### Technology

Shared core in Rust, exposed to both platforms through UniFFI. The core holds
the file operations layer, the protocol, the transport ladder, cryptography,
chunking, and transfer state.

macOS app in Swift and SwiftUI. Android app in Kotlin and Compose.

Native UI is required on both sides anyway. Finder integration and the Android
storage APIs cannot be reached from a cross-platform toolkit. A shared Rust
core avoids writing the hard logic twice.

Budget about one week for UniFFI build plumbing before any feature works.
Cross-compiling for macOS and both Android architectures, generating bindings,
and wiring it into the build is real work. It is not in the phase estimates.

### Two processes, not one

A macOS File Provider extension is a separate process with a memory limit, and
it must work when the main app is not open. So the extension cannot own the
device connection. A host process owns the connection, and the extension talks
to it over an App Group and XPC.

The Rust core must not assume that one process owns everything.

## 5. Scope

### Build

- Device discovery over mDNS, advertised by the phone only, with a random
  instance name and no key material in the TXT record.
- Pairing with commit and reveal, a pairing mode, and confirmation on both
  screens. Then pinned per-device keys.
- Key storage: app-private and backup-excluded on Android, Keychain on macOS.
- Unpairing, which deletes the peer's key and every manifest for it.
- The file operations layer, in both directions. Nine operations, including
  `rename`, `truncate`, and `set_mtime`.
- A shared root on each side, with symlink and special-file refusal.
- Chunking with BLAKE3 hashes, and a persisted session manifest.
- Resume that re-hashes the disk rather than trusting the manifest, and that
  writes to a temporary name until the root hash verifies.
- Pipelined chunk requests with a credit window, so many small files stay fast
  without letting one peer exhaust the other.
- Encrypted transport using Noise, with the per-connection limits in the
  protocol document.
- Finder mount over WebDAV, bound to loopback on a random port, behind a
  per-launch password and a `Host` header check, unmounted when the app quits.
- Android platform work: a foreground service, a Wi-Fi lock, a `MulticastLock`,
  a MediaStore scan after each write, and the all-files-access settings flow.
- USB transport using an adb tunnel, then Android Open Accessory.
- A visible indicator of the active transport and its speed.
- Fuzzing the frame and handshake parsers.

### Cut

- App store submission of any kind. Not needed for personal use.
- Notarization and Developer ID signing.
- Continuous two-way folder sync with conflict handling. Defer it.
- The Storage Access Framework. Use `MANAGE_EXTERNAL_STORAGE` and plain file
  APIs instead. The Play Store is cut, so the policy problem is gone.
- Any Google Play policy work.
- Bluetooth Low Energy.
- Cloud relay and account systems.
- Mid-transfer failover between transports. Session resume replaces it.
- Recursive delete. Deleting a non-empty directory returns an error in
  version 1.

## 6. Phases

Estimates assume one developer with heavy AI assistance. Hours per week is not
yet decided, so these are working weeks, not calendar weeks.

| Order | Scope | Estimate | Risk |
|---|---|---|---|
| 0 | Spike. File Provider hello world, WebDAV mount test, Apple entitlement check. | 3 to 5 days | Low |
| 1 | Rust core: file operations layer, frame codec, Noise with commit-and-reveal pairing, key storage, BLAKE3 chunking, session manifest, resume, per-connection limits. Loopback transport, property tests, and fuzzing. TCP and mDNS. Both apps, including the Android platform work. Push and pull. adb tunnel as a developer transport. | 7 to 9 weeks | Medium |
| 2 | Finder mount over a WebDAV bridge, on top of the file operations layer. | 2 to 4 weeks | Medium |
| 3 | USB reliability. Route decided by the tethering test in section 8. | 0 to 5 weeks | Medium |
| 4 | Optional. File Provider extension, which needs the Apple entitlement question answered. Whole-file deduplication. Hotspot fallback. | Undecided | High |

Phase 2 is the headline feature. It moved ahead of USB for two reasons. It is
the feature I actually need. It is also the only feature here that no free tool
provides.

The order 0 spike settled the phase 2 route. macOS mounts a WebDAV server with
no `sudo`, no TLS, no entitlement, and no Apple Developer Program. Finder
browses it. So the Finder mount ships as a WebDAV bridge over the file
operations layer.

A File Provider extension is the better long-term answer, because it gets
bounded byte ranges and the system caches metadata instead of asking the phone.
It moved to phase 4, and it depends on the Apple entitlement question.

Phase 2 is two to four weeks, not one to two. The bridge carries `LOCK` and
`UNLOCK`, `PROPFIND` at two depths, `MOVE`, `COPY`, `PROPPATCH`, ETags, bounded
open-ended ranges with disconnect detection, a listing cache, metadata probe
handling, authentication, and unmounting on quit. Two of its open questions are
still open.

Phase 1 grew from five to seven weeks to seven to nine. The added work is
pairing with commit and reveal, key storage on both platforms, unpairing,
three more file operations, the per-connection limits, resume, and the Android
platform work. Each item is small. Together they are two weeks.

Phase 3 has no fixed size until the tethering test runs. If Android tethering
presents a usable network interface to the Mac, the USB transport is the
existing TCP transport over that interface, and most of phase 3 disappears.

## 7. Cost

Zero for personal use and for a GitHub portfolio.

On the Mac I build in Xcode and run the app. An app I build myself carries no
quarantine flag, so Gatekeeper never appears.

On Android I install over USB with adb, or press Run in Android Studio. Google
has stated that adb installs stay available alongside the new developer
verification rules.

Costs that do not apply here:

- Apple Developer Program, 99 US dollars per year. Only needed to notarize an
  app for other people to download.
- Google Play Console, 25 US dollars once. Only needed for a Play Store
  listing.

One possible cost. If a free Apple personal team cannot provision App Group
and File Provider entitlements, the File Provider route needs the paid Apple
program. The order 0 spike answers this. If the answer is no, the WebDAV route
is free and still gives a Finder mount.

## 8. Open questions

1. Can a free Apple personal team provision App Group and File Provider
   entitlements on macOS 26? Still open. It needs an Apple ID signed into
   Xcode, which needs a password. It no longer blocks phase 2.
2. Does macOS mount a WebDAV server well enough to browse a phone? Answered.
   Yes. It mounts, it browses, and it fetches ranges rather than whole files.
   It is chatty, it writes `.DS_Store`, and it sends open-ended ranges. All
   three are handled on the Mac side.
3. Can Finder's thumbnail fetches be suppressed? Still open. Two media files
   caused ten content requests, so this matters at scale. This is an entry
   condition for phase 2.
4. Does USB tethering present a usable network interface to the Mac? The plan
   says Android defaults to RNDIS, which macOS does not support. Newer Android
   versions may use NCM, which macOS does support. Five minutes settle it. Plug
   the phone in, turn on USB tethering, and look for a new interface with an
   address in `ifconfig`. If it works, most of phase 3 disappears. Note that
   tethering routes the Mac's internet through the phone.
5. Does FSKit work on macOS 26.6? The risk below cites macOS 26.1 and 26.2, and
   this machine runs 26.6.2. A working FSKit extension would be a real
   filesystem, with no HTTP server, no locking, and no authentication problem.
   A hello-world extension settles it in an afternoon, after the Xcode sign-in.
6. Does mDNS need a `MulticastLock` on Android and a local network permission
   on macOS 26? Confirm during phase 1.

Throughput is deliberately not an open question. USB is a reliability feature,
and the WebDAV route is already chosen. A number would change no decision now.
Phase 2 produces real numbers as a by-product, against a real phone rather than
a fake server on localhost.

## 9. Known risks

- Third-party FSKit extensions were broken on macOS 26.1 and 26.2. That is four
  point releases behind this machine, so the claim is stale. Retest before
  ruling FSKit out. Until then, build the Finder integration on WebDAV.
- The Android app holds access to all shared storage. If the app or its
  transport is ever compromised, everything in shared storage is exposed. The
  fixed shared root in section 4 limits that, and it is the reason the root is
  fixed rather than free.
- The File Provider framework is poorly documented and runs in a memory-limited
  process. It will take longer than it looks.
- macOS's WebDAV client is known to be quirky. The spike measures it rather
  than trusting it.
- Android developer verification starts enforcement on 30 September 2026 in
  Brazil, Indonesia, Singapore and Thailand, and expands from 2027. This does
  not affect personal use over adb. It would affect any future plan to share
  the app widely by sideloading.
- Do not plan on F-Droid as a distribution route. F-Droid has said this rule
  threatens its existence, and the outcome is not settled.

## 10. Portfolio checklist

These cost little and carry more weight than extra features.

1. Write the wire protocol as a document inside the repository. Cover frames,
   handshake, pairing, chunk format, and version negotiation. Record the
   rejected alternatives and the reasons, including QUIC and TLS.
2. Record a demo of about thirty seconds. Start a transfer over Wi-Fi, then
   turn the Wi-Fi off, and show the transfer continuing over the cable. That
   demonstrates the reliability claim. Do not compare speeds, because USB is
   not faster.
3. Show the active transport and its speed in the user interface. It takes a
   few hours and it makes the architecture visible in one screenshot.
4. Use a single repository. Rust core, macOS app, Android app, and the protocol
   document together.
5. Run continuous integration on the Rust core only: tests, clippy, and
   formatting. Cross-compiling both apps in CI costs real setup time and proves
   almost nothing.
6. Write the README for a reader who will never install the app. Explain the
   problem, explain the two layers, and explain one hard decision with its
   reason.

## 11. Deliberately not built

Recording these matters as much as the feature list.

- **QUIC on the local network.** It offers stream multiplexing and connection
  migration. It requires TLS 1.3, which conflicts with the Noise choice, and it
  cannot run over a USB bulk pipe. That means maintaining two security stacks.
  On a LAN with sub-millisecond latency, pipelined requests over one stream
  reach nearly the same result.
- **Content-defined chunking with a rolling hash.** This is what restic and
  borg do, and it pays off for edited documents and disk images. These files are
  photos and videos, which change as whole files or not at all. Fixed chunks
  plus whole-file deduplication captures the real win.
- **A short pairing code over plain Noise `XX`.** An attacker in the middle can
  generate static keys until the code matches, because `XX` sends the
  initiator's static key last. See decision record 6.
- **Recursive delete.** It is the largest single action a peer can trigger, for
  a case the user can do folder by folder.

Fuzzing was on this list and has moved back into scope. The reason given for
cutting it was that the parser never sees hostile bytes after the handshake.
That reason was wrong. Handshake messages are parsed before any peer is
authenticated, and from any host on the network. The resume manifest is parsed
from a file on disk that another process may have edited. A paired device that
is later compromised is authenticated and hostile at the same time.

## 12. Next actions

1. Run the USB tethering test. Five minutes, and it may remove most of phase 3.
2. Sign an Apple ID into Xcode. That answers open question 1, and it also gives
   a stable signing certificate, which phase 1 needs for Keychain storage.
3. Test FSKit on macOS 26.6, once Xcode is signed in.
4. Start phase 1.
