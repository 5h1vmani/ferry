# Ferry

Move files between a Mac and an Android phone. Browse the phone in Finder.

Status: the order 0 spike is done. Phase 1 has not started. There is nothing
to install yet.

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
`stat`, `read(path, offset, length)`, `write(path, offset, bytes)`, `mkdir`,
and `delete`. Either device can serve it. Either device can call it.

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
| USB through an adb tunnel | Developer transport, and a fallback |
| USB through Android Open Accessory | Works with no USB debugging |

USB is not faster than good Wi-Fi. Android Open Accessory runs over USB 2.0
bulk endpoints, and lands in the same throughput range as 5 GHz Wi-Fi. USB is
here for reliability. It works on guest networks that block device discovery,
and it keeps working when the router does not.

A transfer holds an identifier that does not belong to any connection. Both
sides keep a manifest on disk. When the Wi-Fi drops or the cable is pulled, a
new connection resumes the same transfer from that manifest.

## Design decisions

Each decision below has a short record in [docs/decisions](docs/decisions).

- [A shared Rust core, with native user interfaces](docs/decisions/0001-shared-rust-core.md)
- [A file operations layer, not a file sender](docs/decisions/0002-file-operations-layer.md)
- [Noise instead of TLS](docs/decisions/0003-noise-instead-of-tls.md)
- [USB is a reliability feature, not a speed feature](docs/decisions/0004-usb-is-reliability.md)
- [Session resume instead of mid-transfer failover](docs/decisions/0005-session-resume-not-failover.md)

The wire protocol is written up in [docs/protocol.md](docs/protocol.md).

## Repository layout

```
crates/ferry-core   Shared Rust core. Protocol, transports, transfer engine.
docs/               Protocol document and decision records.
spike/              Throwaway probes that answer one question each.
PLAN.md             The build plan, with scope, phases, and cut list.
```

## Licence

MIT. See [LICENSE](LICENSE).
