//! The USB transport, through an adb tunnel.
//!
//! Implemented. The contract below is the specification.
//!
//! # Contract
//!
//! Decision record 9. `adb forward` opens a local TCP port on the Mac that
//! reaches a port on the phone. Everything above the socket is the existing
//! TCP transport, unchanged. This module only finds `adb`, lists devices, and
//! manages forwards.
//!
//! It shells out to the `adb` binary. It never bundles one in version 1.
//!
//! Public shape:
//!
//! ```text
//! /// Look on PATH, then in the default Android SDK location.
//! pub fn find_adb() -> Option<PathBuf>;
//!
//! pub struct Adb { .. }
//! impl Adb {
//!     pub fn new(binary: PathBuf) -> Self;
//!     /// Serial numbers of devices in the "device" state, not "unauthorized".
//!     pub fn devices(&self) -> Result<Vec<String>, AdbError>;
//!     /// Map a local port to a port on the phone. Returns the local port.
//!     pub fn forward(&self, serial: &str, local: u16, remote: u16) -> Result<u16, AdbError>;
//!     pub fn remove_forward(&self, serial: &str, local: u16) -> Result<(), AdbError>;
//! }
//! ```
//!
//! Tests do not need a phone. They point `Adb::new` at a small shell script
//! written into a temporary directory that prints what `adb` would print.
//! One test covers a device in the `unauthorized` state, which must be
//! excluded, because that is the state a phone is in before the person taps
//! "allow" on it.

use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Look on PATH, then in the default Android SDK location.
///
/// PATH is checked first because a person who put a specific `adb` on PATH
/// meant that one to be used. The SDK default is a fallback for a machine
/// that has Android Studio installed but no shell configuration.
///
/// Returns `None` when neither place holds an executable file named `adb`.
#[must_use]
pub fn find_adb() -> Option<PathBuf> {
    find_on_path().or_else(find_in_default_sdk_location)
}

/// Check every directory on PATH for an executable named `adb`.
///
/// An empty PATH element (from a leading, trailing, or doubled separator)
/// means the current directory on POSIX. Skipping it, and then only
/// accepting an absolute candidate, keeps this from ever returning a bare
/// relative name such as `adb`. A relative path would resolve against
/// whatever the working directory happens to be later, at spawn time, which
/// may not be the file that was just checked here.
fn find_on_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("adb"))
        .find(|candidate| candidate.is_absolute() && is_executable_file(candidate))
}

/// Check the default Android SDK install location under the home directory.
///
/// This only knows a default location for macOS and Linux, which are the two
/// platforms Ferry supports. On any other target it reports nothing, since
/// PATH is the only source there.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn find_in_default_sdk_location() -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    let candidate = default_sdk_adb_path(Path::new(&home));
    is_executable_file(&candidate).then_some(candidate)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn find_in_default_sdk_location() -> Option<PathBuf> {
    None
}

/// Where the Android SDK puts `adb` by default, under the home directory.
#[cfg(target_os = "macos")]
fn default_sdk_adb_path(home: &Path) -> PathBuf {
    home.join("Library/Android/sdk/platform-tools/adb")
}

/// Where the Android SDK puts `adb` by default, under the home directory.
#[cfg(target_os = "linux")]
fn default_sdk_adb_path(home: &Path) -> PathBuf {
    home.join("Android/Sdk/platform-tools/adb")
}

/// Whether `path` names a file this process can execute.
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    // Any of the three execute bits is enough. Checking the exact owner,
    // group, or other bit against the current user is not worth the extra
    // code; a permission error will surface soon enough when it is spawned.
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

/// Whether `path` names a file this process can execute.
#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// The reason a call to `adb` did not produce a result.
#[derive(Debug, thiserror::Error)]
pub enum AdbError {
    /// No `adb` binary was found.
    ///
    /// [`find_adb`] returns `None` in that case rather than this error. A
    /// caller that wants a single error type for "no adb anywhere" can fold
    /// that `None` into this variant itself.
    #[error("no adb binary was found")]
    NotFound,
    /// The `adb` process could not be started, or its output could not be
    /// read.
    #[error("running adb failed: {0}")]
    Io(#[from] io::Error),
    /// `adb` ran and exited with a non-zero status.
    #[error("adb exited with status {status}: {stderr}")]
    Failed {
        /// The process exit code. A process killed by a signal has none, so
        /// this is `-1` in that case.
        status: i32,
        /// What `adb` printed to standard error.
        stderr: String,
    },
    /// `adb` did not exit within the timeout.
    ///
    /// A hung adb daemon must not hang Ferry, so the process is killed and
    /// this is returned instead of waiting any longer.
    #[error("adb did not answer within the timeout")]
    Timeout,
    /// `adb` printed something that could not be parsed the way it was
    /// expected to be.
    #[error("could not parse adb output: {0}")]
    Unparseable(String),
    /// The `adb` binary path was not absolute when a command tried to run
    /// it.
    ///
    /// `Command` resolves a relative program name against PATH again at
    /// spawn time. That second lookup could land on a different file than
    /// the one that was checked when this path was chosen, so Ferry refuses
    /// to spawn a relative path rather than risk running the wrong binary.
    #[error("adb binary path is not absolute: {}", .0.display())]
    RelativeBinary(PathBuf),
}

/// A handle to one `adb` binary, used to list devices and manage forwards.
#[derive(Debug)]
pub struct Adb {
    /// Path to the `adb` binary this handle runs.
    binary: PathBuf,
    /// How long to let one `adb` command run before it is killed.
    timeout: Duration,
}

impl Adb {
    /// How long a call to `adb` may run before it is treated as hung.
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

    /// How often a running `adb` command is polled for its exit status.
    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    /// Point Ferry at one `adb` binary, typically the result of [`find_adb`].
    ///
    /// A relative path is canonicalized to an absolute one here, so the file
    /// that gets checked now is still the file that runs later, even if the
    /// working directory changes in between. If canonicalizing fails, for
    /// example because the path does not exist, the path is kept as given;
    /// [`Adb::run`] then refuses to spawn it, since it is still relative.
    #[must_use]
    pub fn new(binary: PathBuf) -> Self {
        let binary = if binary.is_absolute() {
            binary
        } else {
            std::fs::canonicalize(&binary).unwrap_or(binary)
        };
        Self::with_timeout(binary, Self::DEFAULT_TIMEOUT)
    }

    /// Point Ferry at one `adb` binary, with a timeout other than the
    /// default.
    ///
    /// This exists so tests can use a short timeout against a fake `adb`
    /// that hangs on purpose, instead of waiting out the full default.
    fn with_timeout(binary: PathBuf, timeout: Duration) -> Self {
        Self { binary, timeout }
    }

    /// Serial numbers of devices in the `device` state, not `unauthorized`
    /// or `offline`.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] when `adb` cannot be run, exits with a failure
    /// status, or does not answer within the timeout.
    pub fn devices(&self) -> Result<Vec<String>, AdbError> {
        let stdout = self.run(&["devices", "-l"])?;
        Ok(parse_devices(&stdout))
    }

    /// Map a local port to a port on the phone. Returns the local port.
    ///
    /// When `local` is `0`, the operating system picks a free port and
    /// `adb` prints it; that printed port is parsed out and returned.
    /// Otherwise `local` is returned unchanged, since `adb` prints nothing
    /// useful for a port that was already chosen.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] when `adb` cannot be run, exits with a failure
    /// status, does not answer within the timeout, or, when `local` is `0`,
    /// prints something that does not parse as a port number.
    pub fn forward(&self, serial: &str, local: u16, remote: u16) -> Result<u16, AdbError> {
        let local_spec = format!("tcp:{local}");
        let remote_spec = format!("tcp:{remote}");
        let stdout = self.run(&[
            "-s",
            serial,
            "forward",
            local_spec.as_str(),
            remote_spec.as_str(),
        ])?;
        if local != 0 {
            return Ok(local);
        }
        let printed = stdout.trim();
        printed
            .parse()
            .map_err(|_| AdbError::Unparseable(printed.to_string()))
    }

    /// Remove a forward earlier created by [`Adb::forward`].
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] when `adb` cannot be run, exits with a failure
    /// status, or does not answer within the timeout.
    pub fn remove_forward(&self, serial: &str, local: u16) -> Result<(), AdbError> {
        let local_spec = format!("tcp:{local}");
        self.run(&["-s", serial, "forward", "--remove", local_spec.as_str()])?;
        Ok(())
    }

    /// Run `adb` with `args` and return what it printed to standard output.
    fn run(&self, args: &[&str]) -> Result<String, AdbError> {
        // `Command::new` resolves a relative program name against PATH
        // again, at spawn time. That second lookup is not necessarily the
        // file that was checked when this path was chosen, so a relative
        // path is refused instead of trusted.
        if !self.binary.is_absolute() {
            return Err(AdbError::RelativeBinary(self.binary.clone()));
        }

        let mut child = Command::new(&self.binary)
            .args(args)
            // Ferry never has input for adb. Closing stdin keeps the child
            // from ever waiting on the parent's.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Both pipes are drained on background threads while the child is
        // still running, not after. A child that writes more than one pipe
        // buffer blocks on that write until something reads the other end.
        // Reading only after the child has exited would then deadlock: the
        // parent is waiting for the child to exit, and the child is waiting
        // for the parent to read.
        let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
        let stderr_reader = child.stderr.take().map(spawn_pipe_reader);

        let status = self.wait_with_timeout(&mut child);

        // The readers are joined whether the wait succeeded or timed out.
        // On a timeout, `wait_with_timeout` has already killed and waited
        // for the child, so both pipes are already at end of file and these
        // joins return right away.
        let stdout_bytes = join_pipe_reader(stdout_reader);
        let stderr_bytes = join_pipe_reader(stderr_reader);

        let status = status?;

        let stdout = bytes_to_output(stdout_bytes)?;
        let stderr = bytes_to_output(stderr_bytes)?;

        if status.success() {
            Ok(stdout)
        } else {
            Err(AdbError::Failed {
                status: status.code().unwrap_or(-1),
                stderr,
            })
        }
    }

    /// Wait for `child` to exit, killing it if it runs past `self.timeout`.
    ///
    /// This polls [`Child::try_wait`] instead of calling the blocking
    /// [`Child::wait`], because a wait with no deadline is exactly what lets
    /// a hung adb daemon hang Ferry.
    fn wait_with_timeout(&self, child: &mut Child) -> Result<ExitStatus, AdbError> {
        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                // The kill or the wait can fail if the process exited in the
                // instant between the check above and here. That race is
                // harmless, so the result is not checked.
                let _ = child.kill();
                let _ = child.wait();
                return Err(AdbError::Timeout);
            }
            thread::sleep(Self::POLL_INTERVAL);
        }
    }
}

/// Bytes kept from one child pipe before the rest of it is thrown away.
///
/// A child that will not stop printing must not be able to make Ferry hold
/// an unbounded amount of memory. A well-behaved `adb` never gets close to
/// this, so the cap only matters for a broken or malicious one.
const MAX_CAPTURED_OUTPUT: usize = 1024 * 1024;

/// Read `pipe` to end on its own thread, capped at [`MAX_CAPTURED_OUTPUT`].
///
/// This has to run while the child is still alive, not after. See the
/// comment in [`Adb::run`] for why reading only after the child exits can
/// deadlock.
fn spawn_pipe_reader<R>(mut pipe: R) -> thread::JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            // Once `captured` reaches the cap, `remaining` is `0` and this
            // keeps reading into `buffer` without growing `captured` any
            // further. The pipe still gets drained either way, which is the
            // point: a chatty child must not be able to block on a full
            // pipe just because Ferry stopped keeping its output.
            let remaining = MAX_CAPTURED_OUTPUT - captured.len();
            captured.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        captured
    })
}

/// Join a reader thread started by [`spawn_pipe_reader`], if there was one.
///
/// There is nothing more useful to do with a thread that panicked than to
/// treat it as having captured nothing, so a join failure is not reported as
/// an error.
fn join_pipe_reader(reader: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

/// Turn captured pipe bytes into the `String` the rest of this module wants.
///
/// `adb` output is expected to be UTF-8. Bytes that are not are reported the
/// same way a failed read would be, since [`Read::read_to_string`] used to
/// be the thing doing this check before pipes were read on their own
/// threads.
fn bytes_to_output(bytes: Vec<u8>) -> Result<String, AdbError> {
    String::from_utf8(bytes)
        .map_err(|error| AdbError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))
}

/// Pull the serials in the `device` state out of `adb devices -l` output.
///
/// The first line is a header and is skipped. Each remaining line starts
/// with a serial, then whitespace, then a state; the fields after the state
/// are ignored. Only `device` is kept, which excludes `unauthorized` (a
/// phone that has not had "allow" tapped on it yet) and `offline`.
fn parse_devices(output: &str) -> Vec<String> {
    output
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?;
            let state = fields.next()?;
            (state == "device").then(|| serial.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Adb, AdbError, find_adb};
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    /// Serializes tests that change PATH or the current directory.
    ///
    /// Both are process-wide state, and cargo runs tests on several threads
    /// of the same process by default, so two such tests running at once
    /// would corrupt each other's environment.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Restores PATH and the working directory when it drops, even if the
    /// test panics first.
    ///
    /// PATH and the working directory are process-wide. A test that changes
    /// either one has to put it back, and Rust runs destructors during a
    /// panic's unwind, so a `Drop` impl is the way to make that happen
    /// regardless of how the test ends.
    struct EnvGuard {
        path: Option<OsString>,
        dir: PathBuf,
    }

    impl EnvGuard {
        /// Record the current PATH and working directory, to restore later.
        fn capture() -> Self {
            Self {
                path: env::var_os("PATH"),
                dir: env::current_dir().expect("read the current directory"),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // Safety: the caller holds `ENV_MUTEX` for as long as this guard
            // lives, which keeps any other test from reading or writing PATH
            // at the same time.
            #[allow(unsafe_code)]
            unsafe {
                match &self.path {
                    Some(value) => env::set_var("PATH", value),
                    None => env::remove_var("PATH"),
                }
            }
            // The working directory has no equivalent of "unset"; if it was
            // readable at capture time, setting it back is expected to work.
            let _ = env::set_current_dir(&self.dir);
        }
    }

    /// A fresh directory under the system temp directory, unique to one call.
    ///
    /// The counter keeps parallel tests from ever sharing a directory.
    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            env::temp_dir().join(format!("ferry-adb-test-{label}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create a temp dir for the fake adb");
        dir
    }

    /// Write `script_body` as an executable `adb` shell script into `dir`.
    ///
    /// Returns the script's path, ready to pass to [`Adb::new`].
    fn write_fake_adb(dir: &Path, script_body: &str) -> PathBuf {
        let path = dir.join("adb");
        let mut file = fs::File::create(&path).expect("create the fake adb script");
        write!(file, "#!/bin/sh\n{script_body}").expect("write the fake adb script");
        drop(file);
        let mut permissions = fs::metadata(&path)
            .expect("stat the fake adb script")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod the fake adb script");
        path
    }

    /// Write a fake `adb` into its own fresh temp directory.
    ///
    /// Use [`write_fake_adb`] directly instead when a test also needs to
    /// read a file the script wrote, since it needs the directory too.
    fn fake_adb(label: &str, script_body: &str) -> PathBuf {
        write_fake_adb(&unique_temp_dir(label), script_body)
    }

    #[test]
    fn devices_returns_only_serials_in_the_device_state() {
        let binary = fake_adb(
            "devices",
            "printf 'List of devices attached\\nAAA1\\tdevice product:foo\\nBBB2\\tunauthorized\\nCCC3\\toffline\\n\\n'\n",
        );
        let adb = Adb::new(binary);
        let serials = adb.devices().expect("the fake adb should succeed");
        assert_eq!(serials, vec!["AAA1".to_string()]);
    }

    #[test]
    fn devices_with_only_a_header_is_empty() {
        let binary = fake_adb("devices-header-only", "echo 'List of devices attached'\n");
        let adb = Adb::new(binary);
        let serials = adb.devices().expect("the fake adb should succeed");
        assert!(serials.is_empty());
    }

    #[test]
    fn forward_with_an_explicit_port_passes_the_right_arguments_and_returns_it() {
        let dir = unique_temp_dir("forward-explicit");
        let argv_path = dir.join("argv.txt");
        // The path is written directly into the script, as a literal. Using
        // `dirname "$0"` to find it at run time would work on a real shell,
        // but it calls an external program for no reason when the path is
        // already known here.
        let script = format!("echo \"$@\" > {}\n", argv_path.display());
        let binary = write_fake_adb(&dir, &script);
        let adb = Adb::new(binary);

        let port = adb
            .forward("SERIAL1", 6600, 7000)
            .expect("the fake adb should succeed");
        assert_eq!(port, 6600);

        let argv = fs::read_to_string(&argv_path).expect("read the recorded argv");
        assert_eq!(argv.trim(), "-s SERIAL1 forward tcp:6600 tcp:7000");
    }

    #[test]
    fn forward_with_port_zero_parses_the_port_adb_prints() {
        let binary = fake_adb("forward-zero", "echo 45231\n");
        let adb = Adb::new(binary);
        let port = adb
            .forward("SERIAL1", 0, 7000)
            .expect("the fake adb should succeed");
        assert_eq!(port, 45_231);
    }

    #[test]
    fn a_nonzero_exit_status_is_reported_with_its_stderr() {
        let binary = fake_adb("failure", "echo boom 1>&2\nexit 7\n");
        let adb = Adb::new(binary);
        match adb.devices() {
            Err(AdbError::Failed { status, stderr }) => {
                assert_eq!(status, 7);
                assert_eq!(stderr.trim(), "boom");
            }
            other => panic!("expected AdbError::Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_hung_adb_times_out_instead_of_blocking_forever() {
        let binary = fake_adb("timeout", "sleep 30\n");
        // A short timeout, so this test does not itself take 10 seconds.
        let adb = Adb::with_timeout(binary, Duration::from_millis(200));
        match adb.devices() {
            Err(AdbError::Timeout) => {}
            other => panic!("expected AdbError::Timeout, got {other:?}"),
        }
    }

    #[test]
    #[allow(unsafe_code)]
    fn find_adb_returns_none_when_path_and_home_hold_nothing() {
        let _lock = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let empty_home = unique_temp_dir("find-adb-empty-home");
        let original_path = env::var_os("PATH");
        let original_home = env::var_os("HOME");

        // Safety: `ENV_MUTEX` above keeps this from running alongside
        // another test that reads or writes PATH or HOME, and both are
        // restored before this test returns.
        unsafe {
            env::set_var("PATH", "");
            env::set_var("HOME", &empty_home);
        }

        let found = find_adb();

        // Safety: restoring the variables this same test just changed.
        unsafe {
            match original_path {
                Some(value) => env::set_var("PATH", value),
                None => env::remove_var("PATH"),
            }
            match original_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
        }

        assert_eq!(found, None);
    }

    #[test]
    fn a_child_that_prints_more_than_a_pipe_buffer_does_not_deadlock() {
        let _lock = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A pipe buffer is a few tens of KiB on every platform Ferry
        // supports, so 256 KiB on stdout and 256 KiB on stderr both overflow
        // it. Before the fix, `Adb::run` only read a pipe after the child
        // had already exited, so a child stuck writing to a full pipe would
        // never exit, and `devices()` would run out the full timeout.
        let binary = fake_adb(
            "pipe-buffer",
            // Absolute paths, because another test in this binary rewrites PATH for
            // the whole process while it runs. A real header line, because the
            // parser skips the first line and must not skip the device.
            "printf 'List of devices attached\\n'\n/usr/bin/yes | /usr/bin/head -c 262144\n/usr/bin/yes | /usr/bin/head -c 262144 1>&2\nprintf 'AAA1\\tdevice product:foo\\n'\nexit 0\n",
        );
        // A deadlocked child hits this budget and returns Timeout, which fails the
        // assertion below. A slow but working child still returns the device.
        // Asserting the outcome instead of the clock keeps a loaded machine from
        // failing a test the code passes.
        let adb = Adb::with_timeout(binary, Duration::from_secs(8));
        let serials = adb.devices().expect("the fake adb should succeed");
        assert_eq!(serials, vec!["AAA1".to_string()]);
    }

    #[test]
    fn an_empty_path_element_never_yields_a_relative_binary() {
        let _lock = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _guard = EnvGuard::capture();

        let dir = unique_temp_dir("empty-path-element");
        write_fake_adb(&dir, "printf 'List of devices attached\\n'\n");
        env::set_current_dir(&dir).expect("switch to the temp dir with the fake adb");

        // Safety: `ENV_MUTEX` above keeps this from running alongside
        // another test that reads or writes PATH, and `_guard` restores the
        // original PATH and working directory when this test ends, even if
        // an assertion below panics.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var("PATH", "/nonexistent-a::/nonexistent-b");
        }

        let found = find_adb();

        if let Some(path) = found {
            assert!(
                path.is_absolute(),
                "find_adb returned a relative path: {path:?}"
            );
        }
    }

    #[test]
    fn an_adb_built_from_a_relative_path_is_refused() {
        let _lock = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let binary = PathBuf::from("adb");
        if std::fs::canonicalize(&binary).is_ok() {
            // The current directory happens to hold a file named `adb`, so
            // `Adb::new` would canonicalize this to an absolute path rather
            // than keep it relative. That is the correct behavior, but it
            // means this particular case does not apply right now.
            return;
        }

        let adb = Adb::new(binary);
        match adb.devices() {
            Err(AdbError::RelativeBinary(path)) => assert_eq!(path, PathBuf::from("adb")),
            other => panic!("expected AdbError::RelativeBinary, got {other:?}"),
        }
    }
}
