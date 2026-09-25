# Security policy

## Scope

This policy covers:

- Device pairing.
- The Noise transport, which encrypts every connection after pairing.
- The file operations server, which either device runs to serve its
  shared folders.
- The local WebDAV server that mounts a phone's shared folders in Finder.

## Reporting a vulnerability

Report a vulnerability privately through GitHub's private vulnerability
reporting, on this repository's Security tab. Do not open a public issue
for a security problem.

This is a single-maintainer, experimental project. Reports get a best
effort response, with no promised response time.

## The security model

Two devices pair once, over a short confirmation code or a QR code, and
each then holds the other's Noise static public key. Every later
connection uses that pinned key, so no certificate authority and no
public key infrastructure sit in the design. Pairing itself defends
against an active attacker with a commit-and-reveal step, described in
[decision record 6](docs/decisions/0006-pairing-commit-and-reveal.md). The
choice of Noise over TLS is explained in
[decision record 3](docs/decisions/0003-noise-instead-of-tls.md). The full
wire format, including the handshake and the frame layout, is in
[docs/protocol.md](docs/protocol.md).

The WebDAV server binds to loopback only, behind a random per-launch
password, so nothing beyond the Mac itself can reach it. A paired device
can read and write inside its granted shared folders, and nothing else.
