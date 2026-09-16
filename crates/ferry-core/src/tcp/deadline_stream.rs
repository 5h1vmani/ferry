//! [`DeadlineStream`], moved out of `tcp.rs` to keep that file shorter.
//! Nothing here changed when it moved: same fields, same methods, same doc
//! comments.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A `TcpStream` bounded by one deadline that shrinks with every call.
///
/// A plain socket timeout limits one call to `read` or `write`. Code such as
/// `read_exact` calls `read` in a loop. A peer that sends one byte per
/// timeout window resets the limit every time, and can hold the socket open
/// forever. This wrapper works out the time left before every call, and sets
/// the socket timeout to that. The whole sequence of calls is then bounded by
/// one deadline, no matter how many calls it takes.
///
/// `armed` is shared with the code that built this stream. The Noise
/// handshake keeps whatever stream it is given for the life of the
/// connection, so this wrapper cannot be swapped back out for a plain
/// `TcpStream` once the handshake has taken it. Turning `armed` off has the
/// same effect: later calls skip the deadline and go straight to the socket,
/// which is exactly what a plain `TcpStream` would do.
#[derive(Debug)]
pub(super) struct DeadlineStream {
    pub(super) stream: TcpStream,
    pub(super) deadline: Instant,
    pub(super) armed: Arc<AtomicBool>,
}

impl DeadlineStream {
    /// The time left before the deadline.
    ///
    /// Returns a `TimedOut` error once the deadline has passed, rather than
    /// a zero duration, because a zero-length socket timeout is not a valid
    /// one.
    fn time_left(&self) -> io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the handshake did not finish before its deadline",
            ));
        }
        Ok(left)
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.armed.load(Ordering::SeqCst) {
            let left = self.time_left()?;
            self.stream.set_read_timeout(Some(left))?;
        }
        self.stream.read(buf)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.armed.load(Ordering::SeqCst) {
            let left = self.time_left()?;
            self.stream.set_write_timeout(Some(left))?;
        }
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
