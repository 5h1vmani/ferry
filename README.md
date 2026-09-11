# Ferry

Move files between a Mac and an Android phone. Browse the phone in Finder.

Every cut in a transfer resumes and refetches at most one chunk. A test cuts
the wire after every single byte of a transfer, 4718 cut points, and proves
it. See `crates/ferry-runtime/tests/resume_sweep.rs`.

Status: phase 1 is done and has run on a Mac and a Pixel 3 XL. Pairing over
Wi-Fi, a file copied, and the cable all work. On 11 September 2026 the Mac
app took its designed screens, and the engine gained named shared folders,
folder copies as one batch, and an access log on both sides. The fifteen
engine changes behind the screens are in `docs/engine-contract.md`, with
their status. Later that day the engine gained a manifest request so every
pull verifies from its first byte, automatic photo import, push, the
Finder mount over WebDAV with saves that send only changed chunks, and
QR pairing on the Mac side. Each was audited and the findings fixed. None
of it has run on real devices yet; `docs/manual-checks.md` task 4 is the
list. The phone's screens for the new features wait for a Kotlin design
pass. Nothing is packaged for install yet. To build and run it, follow
`docs/manual-checks.md` task 3.

## The problem

Google discontinued Android File Transfer for Mac in May 2024 and shipped no
replacement. Quick Share came to Windows but never to macOS. Apple will not
open AirDrop to Android. No vendor covers this gap.

LocalSend covers sending files over the local network, and it does that well.
No free tool lets a Mac user browse an Android phone inside Finder. That is the
gap Ferry fills.

## How it works

Ferry is built as two layers, not as a file sender.

**The file operations layer** is a small set of remote operations: `list`,
`stat`, `read(path, offset, length)`, `write(path, offset, bytes)`, `truncate`,
`rename`, `set_mtime`, `mkdir`, and `delete`. Either device can serve it.
Either device can call it.

**The transfer engine** sits on top. Pushing a file is repeated `write`.
Pulling a file is repeated `read`. Both directions share one protocol, one
chunking scheme, and one resume path.

Finder integration falls out of the same layer. Finder asks for a directory
listing, then for byte ranges of a file. That is exactly what the file
operations layer already provides.

## Transports

Ferry uses whichever path works, and shows which one is active.

| Transport | Role |
|---|---|
| Wi-Fi on the local network, over mDNS and TCP | Default path |
| USB through an adb tunnel | The USB transport |

USB is not faster than good Wi-Fi. It lands in the same throughput range as
5 GHz Wi-Fi. USB is here for reliability. It works on guest networks that block device discovery,
and it keeps working when the router does not.

A transfer holds an identifier that does not belong to any connection. Both
sides keep a manifest on disk. When the Wi-Fi drops or the cable is pulled, a
new connection resumes the same transfer from that manifest.

The manifest is a hint, not the truth. On resume the receiver re-hashes what it
already holds, because a file on disk can be edited by anything on the machine.

## Design decisions

Each decision below has a short record in [docs/decisions](docs/decisions).

- [A shared Rust core, with native user interfaces](docs/decisions/0001-shared-rust-core.md)
- [A file operations layer, not a file sender](docs/decisions/0002-file-operations-layer.md)
- [Noise instead of TLS](docs/decisions/0003-noise-instead-of-tls.md)
- [USB is a reliability feature, not a speed feature](docs/decisions/0004-usb-is-reliability.md)
- [Session resume instead of mid-transfer failover](docs/decisions/0005-session-resume-not-failover.md)
- [Commit and reveal during pairing](docs/decisions/0006-pairing-commit-and-reveal.md)
- [Blocking input and output, not async](docs/decisions/0007-blocking-io-not-async.md)
- [The Finder mount uses WebDAV, not FSKit](docs/decisions/0008-webdav-not-fskit.md)
- [USB runs over the adb tunnel, not Open Accessory or MTP](docs/decisions/0009-usb-over-adb-not-aoa-or-mtp.md)

The wire protocol is written up in [docs/protocol.md](docs/protocol.md).

## Repository layout

```
crates/ferry-core      Protocol, transports, filesystem, transfer engine.
crates/ferry-runtime   The Engine both apps link, exposed through UniFFI.
macos/                 The Mac app. XcodeGen builds it from project.yml.
android/               The phone app. Gradle with a version catalog.
design/                Colours, tokens, and error words. Generated into both apps.
docs/                  Protocol, decisions, jobs, IA, components, voice, audits.
scripts/               Generators and the environment script.
spike/                 Throwaway probes that answer one question each.
PLAN.md                The build plan, with scope, phases, and cut list.
```

## Licence

MIT. See [LICENSE](LICENSE).
