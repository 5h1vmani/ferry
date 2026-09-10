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

The gap is not abstract. On 10 September 2026 the tool in daily use here,
Android File Transfer over USB, showed a warning that its support for Apple
silicon is ending. Google discontinued it in 2024 and it is now stopping
altogether. That is the transfer this project has to replace first.

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
| USB with an adb tunnel | USB debugging on, which it already is | Build. This is the USB transport. Reuses the TCP code. |
| USB with Android Open Accessory | Plug in, accept a prompt on the phone | Deferred to the optional phase. Portfolio only. See decision record 9. |
| USB with MTP | Plug in | Not built. It cannot write part of a file, allows one session, and serves only the cable. OpenMTP already covers plain USB transfer. See decision record 9. |
| USB tethering | Turn tethering on | Tested. No. This phone runs Android 12, which tethers over RNDIS, and macOS has no RNDIS driver. |
| Phone local-only hotspot | Mac drops its Wi-Fi and loses internet | Defer. Fallback when the LAN blocks discovery. |
| Wi-Fi Direct or AWDL | Not applicable | Cannot build. macOS exposes no public API. |
| Bluetooth Low Energy | None | Cut. mDNS covers the network case. USB covers the cable case. |
| Cloud relay | Needs a code or an account | Out of scope. |

### The USB decision

The USB transport is the adb tunnel. Open Accessory is deferred and MTP is not
built. Decision record 9 gives the reasons, which replace a slogan the earlier
plan used in place of an argument.

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

### Target devices

The phone is a Google Pixel 3 XL on Android 12, and Android 12 is the last
version it will ever run. The Mac runs macOS 26.6.2 on Apple silicon.

The Android app targets the current Android version and sets its minimum to
Android 12, which is API level 31. Nothing in this plan needs a newer API.
Open Accessory, network service discovery, all files access, and foreground
services all exist on Android 12. So building for current Android costs this
phone nothing, and a newer phone gains nothing it cannot already do.

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
- A `hello` exchange as the first frame after every handshake, carrying the
  device's display name. The mDNS name is random and the adb serial is a
  number, so nothing else can put a name on the Devices screen. Found by the
  information architecture in `docs/ia.md`.
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
- USB transport over an adb tunnel.
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
| 0 | Spike. Done. See `docs/spike-0-findings.md`. | Done | Done |
| 1 | Rust core, the engine, both apps, mDNS, TCP, the adb USB transport, key storage, pairing. Pull only. Done and run on real devices on 10 September 2026. What remains is a UX pass and one test. See section 12. | Done | Done |
| 2 | The runtime limits, then one-way photo import with content skip, then the Finder mount over a WebDAV bridge with thumbnail prefetch, push, delta on save, trusted networks, an access log, and the Mac in the Android file picker. | 8 to 12 weeks | Medium |
| 3 | Optional. Per-peer root subset. QR code pairing. File Provider extension, which costs 99 dollars a year. Android Open Accessory, portfolio only. Hotspot fallback. | Undecided | High |

Phase 2 holds the two headline features. Photo import is the job Android
File Transfer's death took away. The Finder mount is the one no free tool
provides. Section 12 lists phase 2 in order, with the reason for each place.

The order 0 spike settled the phase 2 route. macOS mounts a WebDAV server with
no `sudo`, no TLS, no entitlement, and no Apple Developer Program. Finder
browses it. A File Provider extension is the better long-term answer, because
it gets bounded byte ranges and the system caches metadata instead of asking
the phone. It costs 99 dollars a year, so it sits in phase 3.

Phase 2 is two to four weeks. The bridge carries `LOCK` and `UNLOCK`,
`PROPFIND` at two depths, `MOVE`, `COPY`, `PROPPATCH`, ETags, bounded
open-ended ranges with disconnect detection, a listing cache, metadata probe
handling, authentication, and unmounting on quit.

The old phase 3, Android Open Accessory, is gone. The adb tunnel reuses the TCP
transport and lands in phase 1. See decision record 9.

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

One known cost, and it arrives late.

Apple's capability table marks **FileProvider Testing Mode** and **FSKit
Module** as unavailable to a free personal team. Both need the paid Apple
Developer Program. **App Groups** is free, which contradicts most guides
written before 2025.

So phase 3 costs 99 US dollars a year, and everything up to it costs nothing.
The WebDAV Finder mount in phase 2 needs no entitlement at all.

A free personal team also issues a real Apple Development certificate that
lasts about a year and renews itself. That is what phase 1 needs for Keychain
key storage, so no payment is required for that either.

Source: <https://developer.apple.com/help/account/reference/supported-capabilities-macos>,
read 10 September 2026.

## 8. Open questions

1. Can a free Apple personal team provision App Group and File Provider
   entitlements on macOS 26? Answered from Apple's own capability table, read
   on 10 September 2026. App Groups is free. FileProvider Testing Mode is not,
   and neither is FSKit Module. So phase 3 costs 99 US dollars a year, and
   nothing before it does. A free personal team also gets a real Apple
   Development certificate that lasts about a year, which is what phase 1 needs
   for key storage.
   <https://developer.apple.com/help/account/reference/supported-capabilities-macos>
2. Does macOS mount a WebDAV server well enough to browse a phone? Answered.
   Yes. It mounts, it browses, and it fetches ranges rather than whole files.
   It is chatty, it writes `.DS_Store`, and it sends open-ended ranges. All
   three are handled on the Mac side.
3. Can Finder's thumbnail fetches be suppressed? Still open, and the plan
   now answers it the other way: serve them. A JPEG carries its preview in
   the first 64 KB, so the bridge prefetches that head of every image after
   a listing, through the transfer worker pool. See section 12. To be proved
   during the Finder mount.
4. Does USB tethering present a usable network interface to the Mac? Answered
   on 10 September 2026. No. The phone is a Pixel 3 XL on Android 12, which
   tethers over RNDIS, and macOS has no driver for it. Phase 3 stays.
5. Does mDNS need a `MulticastLock` on Android and a local network permission
   on macOS 26? Answered on 10 September 2026 by the first device run. macOS
   26 asks for local network access once, and the app declares the Bonjour
   service and a usage description for it. The Pixel 3 XL advertised and was
   found with no `MulticastLock` held. Another phone may filter multicast;
   add the lock if a phone is not found.

Throughput is deliberately not an open question. USB is a reliability feature,
and the WebDAV route is already chosen. A number would change no decision now.
Phase 2 produces real numbers as a by-product, against a real phone rather than
a fake server on localhost.

## 9. Known risks

- FSKit is not usable for this yet. Three separate faults are on public record,
  the most recent report is dated 22 June 2026 on macOS 26.5.1, and no source
  confirms a fix in 26.6. FSKit does work for a read-only block device, which
  is a much easier case than a phone. Build the Finder integration on WebDAV.
  See decision record 8 for the sources.
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

## 12. Where phase 1 stands

Done, 10 September 2026:

- `crates/ferry-core`: every protocol piece, the real filesystem, TCP with
  its limits, discovery, the peer store, the adb tunnel, and the hello
  exchange. Audited once, six findings fixed. See `docs/audit-1.md`.
- `crates/ferry-runtime`: the Engine both apps link, behind `UniFFI`. Two
  engines pair and move a file in one process in under a second. Builds as a
  shared library for Android. Audited once, fourteen findings fixed. See
  `docs/audit-2.md`.
- Both apps are wired to the engine and build. The Mac links the runtime as
  a static library and keeps its key in the Keychain. The phone packages the
  runtime as a shared library, keeps its key in private storage, runs a
  foreground service while reachable, and asks for all files access on first
  run. The Mac browses the phone's shared storage and pulls a file.
- First run on real devices, 10 September 2026: the phone app installed,
  the Mac app paired with the Pixel over Wi-Fi, copied a photo, and kept
  working over the cable with Wi-Fi off. Two fixes came out of it: where the
  Mac app looks for `adb`, and signing the Mac app with the owner's team so
  the Keychain and the network prompts remember it. See
  `docs/manual-checks.md` task 3.

Not done, in order. Each item names its cost as the owner estimated it and
the reason for its place.

Before phase 2:

1. A UX pass on the moments the first run found. The engine is audited and
   run; the screens were built from the IA before anyone had used them. Daily
   use is the only source of the next engine facts, and the UX is what stops
   daily use. Narrow: the moments named, not a redesign.
2. Deterministic fault injection for resume. Two engines already run in one
   process, and the loopback transport already cuts a stream after N bytes.
   Loop N over a whole transfer and assert that every cut resumes and
   refetches at most one chunk. That is job 3 turned into a test, and it
   locks the resume path before phase 2 changes touch it. Core tests only,
   so it runs beside the UX pass. Days at most.
3. Use it for a week. The outcome metrics in `docs/jobs.md` are the
   acceptance criteria.

Phase 2, in order:

1. The runtime limits: a manifest request so the first pass of a pull
   verifies too, and so a device can be asked "do you hold root hash X"; a
   responder that tries each stored key against the first KK message instead
   of guessing; a way for `stop` to close a socket held inside an encrypted
   stream. The manifest request is the prerequisite for items 2 and 6.
2. Content-addressed skip, then one-way photo import. Before any transfer,
   ask whether the other side holds the root hash; a photo the Mac already
   has is then instant. On top of that a Mac-side rule: when the phone
   appears, pull from DCIM every file the Mac does not hold. Android's
   MediaStore gives "new since last time" without walking the tree. Hashing
   every photo on a 2018 phone is slow, so compare name, size, and time
   first and hash only candidates. One way and additive, so it stays clear
   of the two-way sync cut in section 5. Job 7 in `docs/jobs.md`. One to two
   weeks.
3. The Finder mount over a WebDAV bridge, as section 6 describes. Two to
   four weeks.
4. Thumbnail prefetch through range reads. After a listing, prefetch the
   first 64 KB of every image through the transfer worker pool, so Finder
   shows a folder of 500 photos in seconds. This answers open question 3 by
   serving the requests rather than suppressing them. Days.
5. Push, from the Mac to the phone. Finder save needs it.
6. Chunk-level delta when Finder saves a file. The bridge receives the whole
   file on loopback, hashes it, and sends only the chunks the phone lacks.
   Needs items 1 and 5. Days after those.
7. Advertise only on trusted networks. Remember the Wi-Fi name at pairing
   time and stay silent everywhere else. Job 5 gets stronger and the phone
   saves battery. The engine change is hours. Reading the Wi-Fi name needs
   the location permission on Android and on macOS 14 and later, so the
   real cost is a day and two more prompts on first run, which is why it
   sits after the UX pass has settled the first run.
8. An access log on the phone. The server side already sees every
   operation. Show which Mac read or wrote what, and when. It makes a
   compromised paired Mac visible, which section 9 lists as a risk. Days.
9. The Mac in the Android file picker. Android's DocumentsProvider with
   proxy file descriptors maps almost one to one onto list, stat, read at
   offset, and write at offset. The Mac's shared folder then appears in the
   Files app and in every app's open dialog. It is the mirror of the Finder
   mount on the same layer, so it comes after the mount and reuses its
   decisions. Job 8 in `docs/jobs.md`. One to two weeks.

Phase 3, optional, in no order:

- A per-peer root subset. Share only DCIM to one Mac and everything to
  another. Least privilege on the existing root check. Days. No daily value
  with one Mac, which is why it is optional.
- QR code pairing. Show the Mac's key and address as a QR and scan it with
  the phone camera. It is a second pairing mode beside the six digits, not a
  change to it, and it adds the camera permission. Days.
- The File Provider extension, Android Open Accessory, and the hotspot
  fallback, as section 6 lists them. Whole-file deduplication is covered by
  phase 2 item 2 and is dropped from this list.
