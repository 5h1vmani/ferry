//! One test that uses only the public API, the way the apps will.
//!
//! Every other test lives inside a module and can reach private items. This one
//! compiles as a separate crate, so it fails if a type is not exported. That is
//! the surface the Swift and Kotlin bindings will consume.
//!
//! It also composes the whole stack rather than one layer. Two devices agree a
//! version, pair, encrypt, and move a file. A layer that works alone but not in
//! company fails here.

#![cfg(feature = "testing")]

use std::sync::Arc;
use std::sync::mpsc;

use ferry_core::chunk::{ChunkSize, manifest_from_bytes};
use ferry_core::memfs::MemoryFs;
use ferry_core::noise::{PairingCode, StaticKey, pair_as_initiator, pair_as_responder};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, serve};
use ferry_core::session::{Transfer, pull};
use ferry_core::transport::loopback;
use ferry_core::version::{Role, negotiate};

const SOURCE: &str = "DCIM/Camera/VID_0001.mp4";
const DESTINATION: &str = "Movies/VID_0001.mp4";

fn sample_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31) % 251).unwrap())
        .collect()
}

#[test]
fn two_devices_pair_then_move_a_file_over_the_encrypted_channel() {
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
    assert_eq!(agreed.version, 1);

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
