![The Ferry mark and wordmark](design/logo/ferry-lockup.svg)

# Ferry

[![CI](https://github.com/5h1vmani/ferry/actions/workflows/ci.yml/badge.svg)](https://github.com/5h1vmani/ferry/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Move files between a Mac and an Android phone. Browse the phone in
Finder.

## The problem

Google discontinued Android File Transfer for Mac in May 2024 and shipped
no replacement. Quick Share came to Windows but never to macOS. Apple
will not open AirDrop to Android. No vendor covers this gap.

LocalSend covers sending files over the local network, and it does that
well. No free tool lets a Mac user browse an Android phone inside
Finder. That is the gap Ferry fills.

## Compared with alternatives

| Tool | Browses the phone in Finder | Open source | Price |
|---|---|---|---|
| Ferry | Yes, over a WebDAV mount | Yes, MIT | Free, source only |
| [LocalSend](https://localsend.org) | No; it sends files, it does not mount a drive | Yes | Free |
| [OpenMTP](https://openmtp.ganeshrvel.com) | No; its own two-pane window, not Finder | Yes | Free |
| Android File Transfer | No, and Google discontinued it in May 2024 | No | Free |
| [MacDroid](https://www.macdroid.app) | Yes, over ADB, MTP, or Wi-Fi | No | Free version is limited; the full version is a paid subscription |

## Status

Ferry is experimental. The last confirmed run on a real Mac and a real
phone was on 10 September 2026, before both apps took their current
designed screens. Nothing built since then has run on a device. A real
device run will update this section.

There are no downloads. Ferry is built from source; see "Build from
source" below.

## Requirements

- macOS 14 or later, on Apple silicon (arm64) only.
- Android 12 (API 31) or later, up to API 36, on an arm64-v8a phone.
- For the USB transport: `adb` installed on the Mac, and USB debugging
  turned on on the phone.

## Build from source

See [docs/toolchain.md](docs/toolchain.md) for what to install first.

**Mac:**

```bash
cd macos && xcodegen generate -q && xcodebuild -project Ferry.xcodeproj -scheme Ferry -configuration Debug -derivedDataPath build -quiet build && open build/Build/Products/Debug/Ferry.app
```

This signs the app ad hoc by default, so it builds with no Apple
developer team. See [CONTRIBUTING.md](CONTRIBUTING.md) if you have a
team and want a stable signature across rebuilds.

**Phone:**

```bash
source scripts/env.sh && cd android && ./gradlew assembleDebug
```

This builds `android/app/build/outputs/apk/debug/app-debug.apk`, so you
can build the phone app with no phone attached. Installing it needs a
phone over USB, with `adb`:

```bash
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

## How it works

Ferry is built as two layers, not as a file sender.

**The file operations layer** is a small set of remote operations:
`list`, `stat`, `read(path, offset, length)`, `write(path, offset,
bytes)`, `truncate`, `rename`, `set_mtime`, `mkdir`, and `delete`. Either
device can serve it. Either device can call it.

**The transfer engine** sits on top. Pushing a file is repeated `write`.
Pulling a file is repeated `read`. Both directions share one protocol,
one chunking scheme, and one resume path.

Finder integration falls out of the same layer. Finder asks for a
directory listing, then for byte ranges of a file. That is exactly what
the file operations layer already provides.

### Transports

| Transport | Role |
|---|---|
| Wi-Fi on the local network, over mDNS and TCP | Default path |
| USB through an adb tunnel | The USB transport |

USB is not faster than good Wi-Fi. It is here for reliability: it works
on networks that block device discovery, and it keeps working when the
router does not.

### Resume

A pull over Ferry's own protocol resumes after a cut and refetches at
most one chunk. A test cuts the connection at every byte position during
a pull and checks this bound. The test is slow, so it does not run in
the normal test suite; run it by hand:

```bash
cargo test -p ferry-runtime --test resume_sweep -- --ignored
```

A Finder copy onto the mounted volume does not resume, and neither does
a copy through the phone's Files app.

## Known limits

- A silent link loss stalls a transfer for up to 300 seconds before
  resume starts.
- The chunk-level save on the Finder mount works only when Finder writes
  to the same file path again.
- A plain Finder copy and a copy through the phone's Files app do not
  resume.
- USB needs `adb` installed on the Mac and USB debugging turned on on
  the phone.
- Trusted networks need location permission, because both macOS and
  Android treat the Wi-Fi network name as location data.

## Third-party notices

QR code scanning on the phone uses Google's ML Kit. ML Kit is
proprietary, and it includes Google's own log uploader. Because of
this, F-Droid would not accept the Android app. Ferry's own source code
stays under the MIT licence.

## Design decisions

Each decision below has a short record in
[docs/decisions](docs/decisions). See [SECURITY.md](SECURITY.md) for the
security model built on top of them.

- [A shared Rust core, with native user interfaces](docs/decisions/0001-shared-rust-core.md)
- [A file operations layer, not a file sender](docs/decisions/0002-file-operations-layer.md)
- [Noise instead of TLS](docs/decisions/0003-noise-instead-of-tls.md)
- [USB is a reliability feature, not a speed feature](docs/decisions/0004-usb-is-reliability.md)
- [Session resume instead of mid-transfer failover](docs/decisions/0005-session-resume-not-failover.md)
- [Commit and reveal during pairing](docs/decisions/0006-pairing-commit-and-reveal.md)
- [Blocking input and output, not async](docs/decisions/0007-blocking-io-not-async.md)
- [The Finder mount uses WebDAV, not FSKit](docs/decisions/0008-webdav-not-fskit.md)
- [USB runs over the adb tunnel, not Open Accessory or MTP](docs/decisions/0009-usb-over-adb-not-aoa-or-mtp.md)
- [The Mac app runs without the App Sandbox](docs/decisions/0010-no-mac-sandbox.md)
- [Gestures, not a file manager](docs/decisions/0011-gestures-not-a-file-manager.md)

The wire protocol is written up in [docs/protocol.md](docs/protocol.md).

## Documentation

[docs/README.md](docs/README.md) indexes every file under `docs/`.

## Repository layout

```
crates/ferry-core      Protocol, transports, filesystem, transfer engine.
crates/ferry-runtime   The Engine both apps link, exposed through UniFFI.
macos/                 The Mac app. XcodeGen builds it from project.yml.
android/               The phone app. Gradle with a version catalog.
design/                Colours, tokens, error words, and the logo. Generated into both apps.
docs/                  Protocol, decisions, jobs, IA, components, voice, audits.
scripts/               Generators and the environment script.
spike/                 Throwaway probes that answer one question each.
PLAN.md                The build plan, with scope, phases, and cut list.
```

## Licence

MIT. See [LICENSE](LICENSE).
