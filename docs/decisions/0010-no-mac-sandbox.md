# 10. The Mac app runs without the App Sandbox

Date: 10 September 2026.
Status: accepted.

## Context

The Mac shell was scaffolded with the App Sandbox on and the user selected
files entitlement. That is the right default for an App Store app. Ferry is
not one. It is a personal tool, signed for direct use, and its engine needs
three things the sandbox blocks or makes costly.

| Need | Under the sandbox |
|---|---|
| Run `adb` from Homebrew for USB | The app cannot read or run `/opt/homebrew/bin/adb`, and its `PATH` does not include it. |
| Listen on a TCP port and browse mDNS | Needs the server and client network entitlements. |
| Serve one shared folder for the life of the app | Needs a security scoped bookmark, saved and reopened on every launch. |

## Decision

The sandbox is off. The hardened runtime stays on. The entitlements file is
empty. The shared folder is a plain path in user defaults. The local network
usage description and the Bonjour service list stay in `Info.plist`, because
macOS 15 and later ask every app for local network access, sandboxed or not.

## Consequences

- The app can run `adb`, bind its port, and open the shared folder like any
  command line tool.
- An App Store release would need the sandbox back, with the three rows
  above solved. That is not planned.
- The engine's own limits still hold. It serves one folder, refuses every
  symlink, and answers only a paired key. See `docs/protocol.md`.
