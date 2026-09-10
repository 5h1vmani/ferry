//! Transports, and a loopback transport for tests.
//!
//! The protocol needs one thing from a transport: a reliable, ordered stream of
//! bytes in both directions. TCP gives that. An adb tunnel gives that. A USB
//! bulk endpoint pair gives that.
//!
//! Nothing in the core names a transport trait. Every layer simply asks for
//! [`Read`] plus [`Write`], which `TcpStream` and a USB pipe wrapper already
//! provide.
//!
//! [`loopback`] returns two endpoints joined in memory. It lets the whole
//! protocol run inside one process, with no phone, no cable, and no network.
//! It can also be told to break, which is how resume is tested.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Shared state for one direction of a loopback link.
#[derive(Debug, Default)]
struct PipeState {
    buffer: VecDeque<u8>,
    /// The writing end has gone away. Readers drain, then see end of file.
    closed: bool,
    /// The link failed. Both ends see an error at once.
    broken: bool,
    /// Bytes this direction may still carry before it breaks itself.
    budget: Option<u64>,
}

#[derive(Debug, Default)]
struct Pipe {
    state: Mutex<PipeState>,
    ready: Condvar,
}

impl Pipe {
    fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().expect("loopback lock poisoned");
        if state.broken {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "loopback link broken",
            ));
        }
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "loopback peer gone",
            ));
        }

        // A budget lets a test say "carry this many bytes, then fail".
        let allowed = match state.budget {
            Some(left) => usize::try_from(left).unwrap_or(usize::MAX).min(bytes.len()),
            None => bytes.len(),
        };
        state.buffer.extend(&bytes[..allowed]);
        if let Some(left) = state.budget.as_mut() {
            *left -= allowed as u64;
            if *left == 0 {
                state.broken = true;
            }
        }
        self.ready.notify_all();

        if allowed == 0 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "loopback link broken",
            ));
        }
        Ok(allowed)
    }

    fn read(&self, out: &mut [u8]) -> io::Result<usize> {
        let mut state = self.state.lock().expect("loopback lock poisoned");
        loop {
            if !state.buffer.is_empty() {
                let n = state.buffer.len().min(out.len());
                for slot in out.iter_mut().take(n) {
                    *slot = state.buffer.pop_front().expect("checked not empty");
                }
                return Ok(n);
            }
            if state.broken {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "loopback link broken",
                ));
            }
            if state.closed {
                return Ok(0);
            }
            state = self.ready.wait(state).expect("loopback lock poisoned");
        }
    }

    fn close(&self) {
        let mut state = self.state.lock().expect("loopback lock poisoned");
        state.closed = true;
        self.ready.notify_all();
    }

    fn break_link(&self) {
        let mut state = self.state.lock().expect("loopback lock poisoned");
        state.broken = true;
        self.ready.notify_all();
    }

    fn set_budget(&self, bytes: u64) {
        let mut state = self.state.lock().expect("loopback lock poisoned");
        state.budget = Some(bytes);
    }
}

/// One end of an in-memory link.
///
/// Bytes written here are read by the other endpoint, and the other way round.
/// The buffer is unbounded, so a write never blocks. That keeps tests free of
/// deadlocks. A real transport does not behave this way, which is why the
/// network transports are tested separately.
#[derive(Debug)]
pub struct Endpoint {
    incoming: Arc<Pipe>,
    outgoing: Arc<Pipe>,
    closed: AtomicBool,
}

impl Endpoint {
    /// Break the link in both directions, right now.
    ///
    /// Reads and writes on either endpoint then fail with
    /// [`io::ErrorKind::ConnectionReset`]. This is how a dropped Wi-Fi
    /// connection or an unplugged cable is simulated.
    pub fn break_link(&self) {
        self.incoming.break_link();
        self.outgoing.break_link();
    }

    /// Let this endpoint send `bytes` more, then break the link.
    ///
    /// This is how a transfer is interrupted at a chosen point, so that resume
    /// can be tested at a known offset.
    pub fn break_after_sending(&self, bytes: u64) {
        self.outgoing.set_budget(bytes);
    }
}

impl Read for Endpoint {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.incoming.read(out)
    }
}

impl Write for Endpoint {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.outgoing.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            // Tell the peer that no more bytes are coming.
            self.outgoing.close();
        }
    }
}

/// Create two endpoints joined in memory.
///
/// Whatever one endpoint writes, the other reads.
#[must_use]
pub fn loopback() -> (Endpoint, Endpoint) {
    let a_to_b = Arc::new(Pipe::default());
    let b_to_a = Arc::new(Pipe::default());
    let a = Endpoint {
        incoming: Arc::clone(&b_to_a),
        outgoing: Arc::clone(&a_to_b),
        closed: AtomicBool::new(false),
    };
    let b = Endpoint {
        incoming: a_to_b,
        outgoing: b_to_a,
        closed: AtomicBool::new(false),
    };
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::loopback;
    use std::io::{ErrorKind, Read, Write};

    #[test]
    fn bytes_travel_in_both_directions() {
        let (mut a, mut b) = loopback();
        a.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        b.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");

        b.write_all(b"pong").unwrap();
        a.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[test]
    fn a_dropped_endpoint_gives_the_peer_end_of_file() {
        let (mut a, b) = loopback();
        drop(b);
        let mut buf = [0u8; 8];
        assert_eq!(a.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn breaking_the_link_fails_both_sides() {
        let (mut a, mut b) = loopback();
        a.write_all(b"hello").unwrap();
        a.break_link();

        let mut buf = [0u8; 8];
        assert_eq!(
            b.read(&mut buf).unwrap(),
            5,
            "bytes already sent still arrive"
        );
        assert_eq!(
            b.read(&mut buf).unwrap_err().kind(),
            ErrorKind::ConnectionReset
        );
        assert_eq!(
            a.write(b"more").unwrap_err().kind(),
            ErrorKind::ConnectionReset
        );
    }

    #[test]
    fn a_link_can_be_broken_after_a_chosen_number_of_bytes() {
        let (mut a, mut b) = loopback();
        a.break_after_sending(6);
        assert!(a.write_all(b"123456").is_ok());
        assert_eq!(
            a.write(b"7").unwrap_err().kind(),
            ErrorKind::ConnectionReset
        );

        let mut buf = [0u8; 6];
        b.read_exact(&mut buf).unwrap();
        assert_eq!(
            &buf, b"123456",
            "the bytes sent before the break still arrive"
        );
    }

    #[test]
    fn a_reader_waits_for_a_writer_on_another_thread() {
        let (mut a, mut b) = loopback();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            b.write_all(b"late").unwrap();
        });
        let mut buf = [0u8; 4];
        a.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"late");
        writer.join().unwrap();
    }
}
