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
fn find_on_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join("adb"))
        .find(|candidate| is_executable_file(candidate))
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
    #[must_use]
    pub fn new(binary: PathBuf) -> Self {
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
        let mut child = Command::new(&self.binary)
            .args(args)
            // Ferry never has input for adb. Closing stdin keeps the child
            // from ever waiting on the parent's.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let status = self.wait_with_timeout(&mut child)?;

        let mut stdout = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            pipe.read_to_string(&mut stdout)?;
        }
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            pipe.read_to_string(&mut stderr)?;
        }

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
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

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
        assert!(matches!(adb.devices(), Err(AdbError::Timeout)));
    }

    #[test]
    #[allow(unsafe_code)]
    fn find_adb_returns_none_when_path_and_home_hold_nothing() {
        let empty_home = unique_temp_dir("find-adb-empty-home");
        let original_path = env::var_os("PATH");
        let original_home = env::var_os("HOME");

        // Safety: this test does not run alongside other tests that read or
        // write PATH or HOME, and both are restored before it returns.
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
}
