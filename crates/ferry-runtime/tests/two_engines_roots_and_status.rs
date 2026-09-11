//! Roots, status, and the refusals an engine makes before it starts.
//!
//! `docs/engine-contract.md`, batch C, item 15: a root change reaches an
//! already-connected peer on its very next operation, with no reconnect.
//! The rest of this file covers what the same pair of engines reports about
//! itself: the short code, the status record, and a refused config.

mod common;

use common::engines::{
    Inbox, Recorder, build, build_as, code_of_error, loopback_addr, pair, sample_bytes, static_key,
};

use std::sync::Arc;

use ferry_core::chunk::{ChunkSize, manifest_from_bytes};
use ferry_core::noise::PublicKey;
use ferry_core::ops::OpError;
use ferry_core::path::RemotePath;
use ferry_core::peers::DeviceKind as CoreDeviceKind;
use ferry_core::rpc::{Client, RpcError, exchange_hello};
use ferry_core::tcp;
use ferry_runtime::{Config, DeviceKind, Engine, KeyPair, Root, generate_key};

/// The public half, as `ferry-core` names it.
fn public_key(key: &KeyPair) -> PublicKey {
    let mut out = [0u8; 32];
    out.copy_from_slice(&key.public);
    PublicKey(out)
}

/// `docs/engine-contract.md`, batch C, item 15: a root change must reach an
/// already-connected peer on its very next operation, with no reconnect.
/// `Engine::list` always dials fresh, so this drives one connection by hand,
/// the same way `tests/common/paths.rs` does, to keep it open across the
/// change.
#[test]
fn a_root_change_reaches_an_open_connection_without_a_reconnect() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    // Only pairing itself is needed here: this test drives its own raw
    // connection rather than `mac.engine.list`.
    let _phone_key = pair(&mac, &phone);

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&mac.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Vamana", CoreDeviceKind::Mac)
        .expect("the name exchange should run");
    let mut client = Client::new(stream);

    let root_path = RemotePath::parse("").expect("the empty path is valid");
    let (before, _) = client
        .list(&root_path, 0)
        .expect("the roots should list on the freshly opened connection");
    assert_eq!(
        before.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Root".to_owned()],
        "before the change, only the configured root is listed"
    );

    let renamed_root = tempfile::tempdir().expect("a temporary folder for the renamed root");
    phone
        .engine
        .set_roots(vec![Root {
            name: "Renamed".to_owned(),
            path: renamed_root.path().to_string_lossy().into_owned(),
            writable: true,
        }])
        .expect("set_roots should accept a fresh, valid root");

    // The very next operation, on the very same connection: no reconnect.
    let (after, _) = client
        .list(&root_path, 0)
        .expect("the roots should list again, on the same connection");
    assert_eq!(
        after.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Renamed".to_owned()],
        "the already-open connection sees the new root without reconnecting"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// docs/engine-contract.md item 16a: a manifest request answers with the
/// same manifest `ManifestBuilder` gives over the file's own bytes, once it
/// has travelled a real, paired connection.
#[test]
fn a_manifest_request_crosses_the_wire() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let _phone_key = pair(&mac, &phone);

    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("holiday.bin"), &bytes)
        .expect("the file should write to the shared root");

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&mac.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Vamana", CoreDeviceKind::Mac)
        .expect("the name exchange should run");
    let mut client = Client::new(stream);

    let path = RemotePath::parse("Root/holiday.bin").expect("a valid path");
    let manifest = client
        .manifest(&path)
        .expect("the peer should answer a manifest request");
    let expected = manifest_from_bytes(&bytes, ChunkSize::one_mebibyte());
    assert_eq!(
        manifest, expected,
        "the served manifest must match the file's own bytes"
    );

    let root_path = RemotePath::parse("Root").expect("a valid path");
    let dir_error = client
        .manifest(&root_path)
        .expect_err("a directory has no manifest");
    assert!(
        matches!(dir_error, RpcError::Remote(OpError::IsADirectory)),
        "expected IsADirectory, got {dir_error:?}"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// Batch B and C audit, C6: `set_roots` refuses a bad set, and the roots it
/// already had keep serving.
#[test]
fn a_bad_set_roots_call_is_refused_and_the_old_roots_keep_serving() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone_key = pair(&mac, &phone);

    let outer = tempfile::tempdir().expect("a temporary folder for the outer root");
    let inner_path = outer.path().join("inner");
    std::fs::create_dir(&inner_path).expect("a nested folder for the inner root");

    let error = phone
        .engine
        .set_roots(vec![
            Root {
                name: "Outer".to_owned(),
                path: outer.path().to_string_lossy().into_owned(),
                writable: true,
            },
            Root {
                name: "Inner".to_owned(),
                path: inner_path.to_string_lossy().into_owned(),
                writable: true,
            },
        ])
        .expect_err("a root nested inside another root should be refused");
    assert_eq!(
        code_of_error(&error),
        "RootsError::RootOverlaps",
        "the RootsError code crosses the boundary"
    );

    let entries = mac
        .engine
        .list(phone_key, String::new())
        .expect("the old roots should still serve");
    assert_eq!(
        entries.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Root".to_owned()],
        "a refused set_roots call leaves the previous roots serving"
    );

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
fn the_engine_refuses_what_it_should_and_says_why() {
    let side = build("Vamana");

    let missing = side
        .engine
        .retry("no-such-transfer".to_owned())
        .expect_err("an unknown transfer cannot be retried");
    assert_eq!(code_of_error(&missing), "Runtime::TransferNotFound");

    let stranger = side
        .engine
        .forget("not a key".to_owned())
        .expect_err("an unpaired device cannot be forgotten");
    assert_eq!(code_of_error(&stranger), "Runtime::NotPaired");

    let bad_path = side
        .engine
        .pull("00".repeat(32), "../escape".to_owned(), "a.bin".to_owned())
        .expect_err("a path that climbs out of the root is refused");
    assert_eq!(code_of_error(&bad_path), "PathError::ParentComponent");

    let root_source = side
        .engine
        .pull("00".repeat(32), String::new(), "a.bin".to_owned())
        .expect_err("the shared root has no single file to pull");
    assert_eq!(code_of_error(&root_source), "PathError::Empty");

    let unpaired = side
        .engine
        .pull("00".repeat(32), "a.bin".to_owned(), "a.bin".to_owned())
        .expect_err("an unpaired device cannot be pulled from");
    assert_eq!(code_of_error(&unpaired), "Runtime::NotPaired");

    side.engine.stop();
}

#[test]
fn a_reachable_engine_has_a_four_character_short_code() {
    let phone = build("Pixel 3 XL");

    phone.engine.set_reachable(true);
    let code = phone
        .engine
        .short_code()
        .expect("a reachable engine should show its own short code");
    assert_eq!(code.chars().count(), 4);

    phone.engine.set_reachable(false);
    assert_eq!(
        phone.engine.short_code(),
        None,
        "an unreachable engine shows no short code"
    );

    phone.engine.stop();
}

#[test]
fn status_reports_reachability_listen_port_and_adb_presence() {
    let phone = build("Pixel 3 XL");

    let before = phone.engine.status();
    assert!(!before.reachable, "reachability starts off");
    assert_ne!(before.listen_port, 0, "a started engine has bound a port");
    assert_eq!(
        before.listen_port,
        phone
            .engine
            .listen_addr()
            .expect("the engine has started")
            .port(),
        "status reports the same port the engine bound"
    );
    assert_eq!(
        before.adb_present,
        ferry_core::adb::find_adb().is_some(),
        "status reports whether this machine has adb, same as the engine found at start"
    );

    phone.engine.set_reachable(true);
    assert!(
        phone.engine.status().reachable,
        "status follows set_reachable"
    );

    phone.engine.stop();
}

#[test]
fn a_bad_config_is_refused_before_anything_starts() {
    let data = tempfile::tempdir().expect("a temporary folder");
    let shared = tempfile::tempdir().expect("a temporary folder");
    let download = tempfile::tempdir().expect("a temporary folder");
    let inbox = Arc::new(Inbox::default());
    let make = |name: String, key: KeyPair| {
        Engine::new(
            Config {
                data_dir: data.path().to_string_lossy().into_owned(),
                shared_roots: vec![Root {
                    name: "Root".to_owned(),
                    path: shared.path().to_string_lossy().into_owned(),
                    writable: true,
                }],
                download_dir: download.path().to_string_lossy().into_owned(),
                display_name: name,
                listen_port: 0,
                key,
                kind: DeviceKind::Mac,
            },
            Box::new(Recorder {
                inbox: Arc::clone(&inbox),
            }),
        )
    };

    let short_key = KeyPair {
        private: vec![0u8; 31],
        public: vec![0u8; 32],
    };
    let bad_key = make("Vamana".to_owned(), short_key).err().expect("refused");
    assert_eq!(code_of_error(&bad_key), "NoiseError::BadKeyLength");

    let good_key = generate_key().expect("a fresh key pair");
    let long_name = make("n".repeat(65), good_key).err().expect("refused");
    assert_eq!(code_of_error(&long_name), "Runtime::NameTooLong");
}
