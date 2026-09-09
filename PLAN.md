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
| USB with Android Open Accessory | Plug in, accept a prompt on the phone | Build. No USB debugging needed. |
| USB with MTP | Plug in | Skip. This is what everyone else does badly. |
| Phone local-only hotspot | Mac drops its Wi-Fi and loses internet | Defer. Fallback when the LAN blocks discovery. |
| Wi-Fi Direct or AWDL | Not applicable | Cannot build. macOS exposes no public API. |
| Bluetooth Low Energy | None | Cut. mDNS covers the network case. USB covers the cable case. |
| Cloud relay | Needs a code or an account | Out of scope. |

USB tethering looks attractive but is not reliable. Android defaults to
RNDIS, which macOS does not support. CDC-NCM works on macOS but Android gates
it behind vendor configuration. Do not build on it.

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

Noise protocol, not TLS. The `XX` pattern handles first pairing with a short
confirmation code. The `KK` pattern handles every connection after that, using
the pinned static key of each device.

Reason: the pairing model is "pin the peer's static public key", which is
exactly what Noise does. There is no certificate generation and no PKI
vocabulary in the protocol document. Noise also runs over any byte stream, so
the USB pipe and the TCP socket use identical code. TLS only wins if a browser
needs to connect, and no browser needs to connect.

### Integrity

BLAKE3. It is fast, it parallelises, and its tree structure gives per-chunk
verification and a whole-file root hash from a single pass.

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

- Device discovery over mDNS.
- Pairing with a short confirmation code, then pinned per-device keys.
- The file operations layer, in both directions.
- Chunking with BLAKE3 hashes, and a persisted session manifest.
- Pipelined chunk requests, so many small files stay fast.
- Encrypted transport using Noise.
- Finder mount. Route decided by the order 0 spike.
- USB transport using an adb tunnel, then Android Open Accessory.
- A visible indicator of the active transport and its speed.

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

## 6. Phases

Estimates assume one developer with heavy AI assistance. Hours per week is not
yet decided, so these are working weeks, not calendar weeks.

| Order | Scope | Estimate | Risk |
|---|---|---|---|
| 0 | Spike. File Provider hello world, WebDAV mount test, Apple entitlement check. | 3 to 5 days | Low |
| 1 | Rust core: file operations layer, frame codec, Noise, pairing, BLAKE3 chunking, session manifest. Loopback transport and property tests. TCP and mDNS. Both apps. Push and pull. adb tunnel as a developer transport. | 5 to 7 weeks | Medium |
| 2 | Finder mount over a WebDAV bridge, on top of the file operations layer. | 1 to 2 weeks | Low |
| 3 | Android Open Accessory over USB. Session resume across a dropped transport. | 3 to 5 weeks | Medium |
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
   caused ten content requests, so this matters at scale.
4. Does mDNS need a `MulticastLock` on Android and a local network permission
   on macOS 26? Confirm during phase 1.

Throughput is deliberately not an open question. USB is a reliability feature,
and the WebDAV route is already chosen. A number would change no decision now.
Phase 2 produces real numbers as a by-product, against a real phone rather than
a fake server on localhost.

## 9. Known risks

- Third-party FSKit extensions are broken on macOS 26. This is an Apple bug and
  it was still present in macOS 26.1 and 26.2. Build the Finder integration on
  File Provider or WebDAV, not on FSKit.
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
- **Fuzzing the frame parser.** After the Noise handshake the peer is
  authenticated, so the parser never sees hostile bytes. Property tests on the
  codec round trip catch more real bugs.

## 12. Next actions

1. Sign an Apple ID into Xcode, then answer open question 1. It takes about ten
   minutes and it decides whether phase 4 is free.
2. Start phase 1.
