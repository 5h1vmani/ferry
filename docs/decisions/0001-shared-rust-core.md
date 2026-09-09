# 1. A shared Rust core, with native user interfaces

Date: 9 September 2026.
Status: accepted.

## Context

Ferry needs a macOS app and an Android app. The hard parts are the same on both
sides: the protocol, cryptography, chunking, and transfer state. Writing those
twice would double the bugs.

Finder integration on macOS needs the File Provider framework. Android storage
access needs the platform file APIs. Neither is reachable from a cross-platform
user interface toolkit.

## Decision

Write the core in Rust. Expose it to both platforms through UniFFI. Write the
macOS app in Swift and SwiftUI, and the Android app in Kotlin and Compose.

## Reasons

The hard logic exists once. The parts that must be native stay native.

A single core also gives one place to test the protocol. A loopback transport
in Rust exercises the whole state machine with no phone and no cable.

## Consequences

Build plumbing costs about one week before any feature works. That covers
cross-compiling for macOS and both Android architectures, generating bindings,
and wiring it into both builds.

The core must not assume that one process owns everything. A macOS File
Provider extension runs as a separate process, so the connection lives in a
host process and the extension talks to it.

## Rejected alternative

Flutter on both sides, which is what LocalSend chose. That ships faster and
gives up the Finder experience. The Finder experience is the headline feature
here, so this was rejected.
