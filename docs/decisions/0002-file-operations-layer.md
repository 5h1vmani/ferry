# 2. A file operations layer, not a file sender

Date: 9 September 2026.
Status: accepted.

## Context

Ferry needs to push files, pull files, and let Finder browse the phone. Built
as three features, that is three protocols and three resume paths.

## Decision

Define one small set of remote operations: `list`, `stat`,
`read(path, offset, length)`, `write(path, offset, bytes)`, `mkdir`, and
`delete`. Either side can serve it. Either side can call it.

Build the transfer engine on top. Pushing is repeated `write`. Pulling is
repeated `read`.

## Reasons

Push and pull become one protocol used in two directions. Chunking, integrity
checks, and resume are written once.

Finder integration then falls out of the same layer. A File Provider extension
asks for a directory listing, then for byte ranges of an item. A WebDAV bridge
asks for the same things. Both are thin adapters over this layer.

Building the alternative first is the expensive path. If phase 1 shipped "send
a file blob down a socket", Finder support later would mean inventing this
layer under time pressure, and publishing a second version of the wire
protocol.

## Consequences

Reads and writes carry a byte range from the first version of the protocol.
Finder must list files without downloading them. A whole-file read API cannot
support that.

Paths arrive from a peer, so the core validates them before use. See
`crates/ferry-core/src/path.rs`.

## Rejected alternative

A push-only protocol, which is what LocalSend and Warpinator use. It is simpler
and it cannot support a Finder mount.
