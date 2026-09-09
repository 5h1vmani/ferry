# 5. Session resume instead of mid-transfer failover

Date: 9 September 2026.
Status: accepted.

## Context

Ferry has more than one transport. A transfer should survive when the active
transport dies, for example when Wi-Fi drops or a cable is pulled.

The obvious design is to migrate the live stream to another transport. That is
a hard problem. It needs dead-path detection, a hand-over that loses no bytes,
and protection against writing the same range twice.

## Decision

Do not migrate live streams. Give each transfer an identifier that does not
belong to any connection. Both sides persist a manifest to disk: the chunk
list, and which chunks are confirmed.

When a connection dies, open a new one and resume the same transfer from the
manifest.

## Reasons

The user sees the same result. The transfer continues.

The work is a fraction of the size, and most of it is needed anyway. Resumable
transfers already require a persisted manifest.

Resume also covers cases that failover does not, such as the app being closed
and reopened.

## Consequences

There is a short pause when a transport dies, while the new connection opens
and the two sides compare manifests. That pause is acceptable.

The manifest format becomes part of the wire protocol from version 1.
