# 7. Blocking input and output, not async

Date: 9 September 2026.
Status: accepted.

## Context

The core needs to read and write bytes over TCP, over an adb tunnel, and over
a USB bulk endpoint pair. Rust offers two shapes for that work. One is async,
usually with Tokio. The other is blocking calls with threads.

## Decision

The core asks a transport only for [`std::io::Read`] plus [`std::io::Write`].
Concurrency comes from threads.

## Reasons

The connection count is tiny. Two devices are paired. The macOS WebDAV client
opened five connections during the order 0 spike, and that was the busiest
moment observed. Threads handle five connections without effort.

`TcpStream` already implements `Read` and `Write`. So does a file. A USB bulk
pipe wrapper can. So the same protocol code runs over every transport with no
adapter layer.

Async adds a runtime, coloured functions, and a large dependency tree. It buys
throughput at high connection counts, which this project never reaches.

UniFFI is also simpler with blocking calls. Async across a foreign function
boundary works, but it is more machinery for no gain here.

## Consequences

A connection that is serving requests needs a thread. Pipelining is handled by
a reader thread, a writer thread, and a dispatcher, not by a task scheduler.

Cancellation is explicit. A blocking read stops when the stream closes or when
the code checks a flag between chunks. The File Provider extension cancels
through `NSProgress`, so that flag must be checked between chunks, not only
between whole files.

Blocking calls cannot be interrupted from outside. Any operation that could
hang needs a socket timeout set on the stream itself.

## Rejected alternative

Tokio with `AsyncRead` and `AsyncWrite`. It is the right choice for a server
holding thousands of connections. This is two phones and a laptop.
