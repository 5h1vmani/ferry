//! Two tests that use only the public API, the way the apps will.
//!
//! Every other test lives inside a module and can reach private items. Tests
//! here compile as a separate crate, so they fail if a type is not exported.
//! That is the surface the Swift and Kotlin bindings will consume.
//!
//! Both tests also compose the whole stack rather than one layer. Two
//! devices agree a version, pair, encrypt, and move a file. A layer that
//! works alone but not in company fails here.
//!
//! `two_devices_pair_then_move_a_file_over_the_encrypted_channel` serves
//! `MemoryFs`, the reference filesystem, so it only builds with the
//! `testing` feature on. `two_devices_move_a_file_between_two_real_directories`
//! serves `LocalFs` on two real temporary directories instead, and needs no
//! feature: `LocalFs` ships in every build, and before this test, nothing
//! outside its own module exercised it.

use std::sync::mpsc;

use ferry_core::chunk::{ChunkSize, manifest_from_bytes};
use ferry_core::localfs::LocalFs;
use ferry_core::noise::{PairingCode, StaticKey, pair_as_initiator, pair_as_responder};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, serve};
use ferry_core::session::{Transfer, pull};
use ferry_core::transport::loopback;
use ferry_core::version::{Role, VERSION_MAX, negotiate};

const SOURCE: &str = "DCIM/Camera/VID_0001.mp4";
const DESTINATION: &str = "Movies/VID_0001.mp4";

fn sample_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31) % 251).unwrap())
        .collect()
}

// `MemoryFs` is only compiled in behind the `testing` feature (see
// `ferry_core::memfs`'s own gate), so the test that serves it has to be
// gated the same way. `Arc` and `MemoryFs` are imported inside the function
// rather than at file scope, so nothing here goes unused, and the import
// list stays gated exactly with the code that needs it.
#[cfg(feature = "testing")]
#[test]
fn two_devices_pair_then_move_a_file_over_the_encrypted_channel() {
    use std::sync::Arc;

    use ferry_core::memfs::MemoryFs;

    let bytes = sample_bytes(120_000);

    let phone_files = Arc::new(MemoryFs::new());
    phone_files.insert_file(SOURCE, bytes.clone());

    let mac_files = Arc::new(MemoryFs::new());
    mac_files.insert_dir("Movies");

    let (mut mac_link, mut phone_link) = loopback();
    let (send_code, receive_code) = mpsc::channel::<PairingCode>();

    let phone = std::thread::spawn(move || {
        let agreed = negotiate(&mut phone_link, Role::Responder).expect("version agreed");
        let key = StaticKey::generate().expect("key made");
        let mut paired =
            pair_as_responder(phone_link, &key, &agreed.prologue).expect("pairing finished");
        send_code.send(paired.code).expect("code sent");
        serve(&mut paired.stream, phone_files.as_ref()).expect("served until the peer left");
    });

    let agreed = negotiate(&mut mac_link, Role::Initiator).expect("version agreed");
    assert_eq!(agreed.version, VERSION_MAX);

    let key = StaticKey::generate().expect("key made");
    let paired = pair_as_initiator(mac_link, &key, &agreed.prologue).expect("pairing finished");

    // A person compares these two on the two screens. They must match, and the
    // code must be six digits so a person will actually read it.
    let phone_code = receive_code.recv().expect("code arrived");
    assert_eq!(paired.code, phone_code);
    assert_eq!(paired.code.to_string().len(), 6);

    let manifest = manifest_from_bytes(&bytes, ChunkSize::one_mebibyte());
    let transfer = Transfer::new(
        manifest,
        RemotePath::parse(SOURCE).expect("valid path"),
        RemotePath::parse(DESTINATION).expect("valid path"),
    )
    .expect("transfer started");

    let mut client = Client::new(paired.stream);
    let progress = pull(&mut client, &transfer, mac_files.as_ref()).expect("file arrived");

    assert!(progress.is_complete());
    assert_eq!(progress.bytes_done, bytes.len() as u64);
    assert_eq!(mac_files.file_bytes(DESTINATION), Some(bytes));

    // The partial file must not survive a finished transfer.
    let temporary = transfer.temporary_path().expect("valid temporary path");
    assert!(mac_files.file_bytes(temporary.as_str()).is_none());

    drop(client);
    phone.join().expect("the phone thread ended cleanly");
}

// `MemoryFs` is gated the same way as the pairing test above, and for the
// same reason.
#[cfg(feature = "testing")]
#[test]
fn the_in_memory_filesystem_lists_the_root_over_the_wire() {
    use std::sync::Arc;

    use ferry_core::memfs::MemoryFs;

    let files = Arc::new(MemoryFs::new());
    files.insert_dir("DCIM");
    files.insert_file("DCIM/a.jpg", b"a".to_vec());

    let (client_link, mut server_link) = loopback();
    let server = std::thread::spawn(move || {
        serve(&mut server_link, files.as_ref()).expect("served until the peer left");
    });

    let root = RemotePath::parse("").expect("the empty string is the shared root");
    let mut client = Client::new(client_link);
    let (entries, next_cursor) = client.list(&root, 0).expect("the root should list");
    assert!(
        entries.iter().any(|entry| entry.name == "DCIM"),
        "the folder made above should be in the root's listing"
    );
    assert_eq!(next_cursor, None);

    drop(client);
    server.join().expect("the server thread ended cleanly");
}

/// A directory under the system temp directory, unique to one process, that
/// removes itself when it is dropped, including when a test panics.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ferry-end-to-end-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// `LocalFs` is used by no test outside its own module today, so nothing
// proves it works through the public API the way the apps will call it,
// composed with the rest of the stack. This is the same test as
// `two_devices_pair_then_move_a_file_over_the_encrypted_channel` above, with
// `LocalFs` on two real temporary directories standing in for the phone's
// and the Mac's shared roots instead of `MemoryFs`. It needs no feature:
// `LocalFs` ships in every build.
#[test]
fn two_devices_move_a_file_between_two_real_directories() {
    let bytes = sample_bytes(120_000);

    let phone_root = TempDir::new("phone");
    std::fs::create_dir_all(phone_root.path.join("DCIM/Camera")).unwrap();
    std::fs::write(phone_root.path.join(SOURCE), &bytes).unwrap();
    let phone_files = LocalFs::open(&phone_root.path).expect("phone root opened");

    let mac_root = TempDir::new("mac");
    std::fs::create_dir_all(mac_root.path.join("Movies")).unwrap();
    let mac_files = LocalFs::open(&mac_root.path).expect("mac root opened");

    let (mut mac_link, mut phone_link) = loopback();
    let (send_code, receive_code) = mpsc::channel::<PairingCode>();

    let phone = std::thread::spawn(move || {
        let agreed = negotiate(&mut phone_link, Role::Responder).expect("version agreed");
        let key = StaticKey::generate().expect("key made");
        let mut paired =
            pair_as_responder(phone_link, &key, &agreed.prologue).expect("pairing finished");
        send_code.send(paired.code).expect("code sent");
        serve(&mut paired.stream, &phone_files).expect("served until the peer left");
    });

    let agreed = negotiate(&mut mac_link, Role::Initiator).expect("version agreed");
    assert_eq!(agreed.version, VERSION_MAX);

    let key = StaticKey::generate().expect("key made");
    let paired = pair_as_initiator(mac_link, &key, &agreed.prologue).expect("pairing finished");

    // A person compares these two on the two screens. They must match, and the
    // code must be six digits so a person will actually read it.
    let phone_code = receive_code.recv().expect("code arrived");
    assert_eq!(paired.code, phone_code);
    assert_eq!(paired.code.to_string().len(), 6);

    let manifest = manifest_from_bytes(&bytes, ChunkSize::one_mebibyte());
    let transfer = Transfer::new(
        manifest,
        RemotePath::parse(SOURCE).expect("valid path"),
        RemotePath::parse(DESTINATION).expect("valid path"),
    )
    .expect("transfer started");

    let mut client = Client::new(paired.stream);
    let progress = pull(&mut client, &transfer, &mac_files).expect("file arrived");

    assert!(progress.is_complete());
    assert_eq!(progress.bytes_done, bytes.len() as u64);
    assert_eq!(
        std::fs::read(mac_root.path.join(DESTINATION)).expect("destination file readable"),
        bytes
    );

    // The partial file must not survive a finished transfer.
    let temporary = transfer.temporary_path().expect("valid temporary path");
    assert!(!mac_root.path.join(temporary.as_str()).exists());

    drop(client);
    phone.join().expect("the phone thread ended cleanly");
}

#[test]
fn the_on_disk_filesystem_lists_the_root_over_the_wire() {
    let root_dir = TempDir::new("wire-root");
    std::fs::create_dir(root_dir.path.join("DCIM")).unwrap();
    let files = LocalFs::open(&root_dir.path).expect("root opened");

    let (client_link, mut server_link) = loopback();
    let server = std::thread::spawn(move || {
        serve(&mut server_link, &files).expect("served until the peer left");
    });

    let root = RemotePath::parse("").expect("the empty string is the shared root");
    let mut client = Client::new(client_link);
    let (entries, next_cursor) = client.list(&root, 0).expect("the root should list");
    assert!(
        entries.iter().any(|entry| entry.name == "DCIM"),
        "the folder made above should be in the root's listing"
    );
    assert_eq!(next_cursor, None);

    drop(client);
    server.join().expect("the server thread ended cleanly");
}
