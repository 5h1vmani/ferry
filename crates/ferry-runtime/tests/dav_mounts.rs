//! Starting and stopping one device's `WebDAV` mount.
//!
//! `docs/engine-contract.md`, item 6. These cover what only makes sense
//! once a mount's life is over, or before it begins: the port closing, an
//! unknown device, and the mount path the app reports back.
//!
//! The engines and the folders this file drives live in
//! `tests/common/mod.rs`.

mod common;

use std::net::{SocketAddr, TcpStream};

use ferry_runtime::{DeviceKind, generate_key};

use common::{build_side, loopback_addr, port_of};

#[test]
fn mount_stop_closes_the_port() {
    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key = generate_key().expect("a fresh key pair");
    let mac = build_side(
        "Vamana",
        DeviceKind::Mac,
        mac_key.clone(),
        &[],
        &phone_key,
        "Pixel 3 XL",
        DeviceKind::Phone,
    );
    let phone = build_side(
        "Pixel 3 XL",
        DeviceKind::Phone,
        phone_key.clone(),
        &[("Notes.txt", b"hi")],
        &mac_key,
        "Vamana",
        DeviceKind::Mac,
    );
    phone.engine.set_reachable(true);
    let phone_key_hex = mac
        .engine
        .devices()
        .first()
        .expect("paired")
        .key_hex
        .clone();
    mac.engine.offer_candidate(loopback_addr(&phone));
    mac.engine
        .list(phone_key_hex.clone(), String::new())
        .expect("listing should succeed");

    let endpoint = mac
        .engine
        .mount_start(phone_key_hex.clone())
        .expect("mount_start");
    let port = port_of(&endpoint.url);
    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("a loopback address");
    // The port answers before the stop.
    drop(TcpStream::connect(addr).expect("the bridge should be listening"));

    mac.engine.mount_stop(phone_key_hex);
    assert!(
        TcpStream::connect(addr).is_err(),
        "the port must be closed once mount_stop returns"
    );

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
fn mount_start_on_an_unknown_device_is_not_paired() {
    let mac = build_side(
        "Vamana",
        DeviceKind::Mac,
        generate_key().expect("a fresh key pair"),
        &[],
        &generate_key().expect("a fresh key pair"),
        "Nobody",
        DeviceKind::Phone,
    );
    let error = mac
        .engine
        .mount_start("not-a-real-key-hex".to_owned())
        .expect_err("an unknown device cannot be mounted");
    let ferry_runtime::FerryError::Failed { code, .. } = error;
    assert_eq!(code, "Runtime::NotPaired");
    mac.engine.stop();
}

#[test]
fn set_mount_path_is_read_back_through_device_info() {
    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key = generate_key().expect("a fresh key pair");
    let mac = build_side(
        "Vamana",
        DeviceKind::Mac,
        mac_key,
        &[],
        &phone_key,
        "Pixel 3 XL",
        DeviceKind::Phone,
    );
    let key_hex = mac
        .engine
        .devices()
        .first()
        .expect("paired")
        .key_hex
        .clone();
    assert_eq!(mac.engine.devices()[0].mount_path, None);

    mac.engine
        .set_mount_path(key_hex.clone(), Some("/Volumes/Pixel 3 XL".to_owned()))
        .expect("a paired device accepts a mount path");
    assert_eq!(
        mac.engine.devices()[0].mount_path,
        Some("/Volumes/Pixel 3 XL".to_owned())
    );

    mac.engine
        .set_mount_path(key_hex, None)
        .expect("clearing the path succeeds");
    assert_eq!(mac.engine.devices()[0].mount_path, None);

    mac.engine.stop();
}
