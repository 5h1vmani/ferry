//! Item 13's thirty day retention, and the getter that reports it.
//!
//! `docs/engine-contract.md`, item 13, "Thirty days": a log that quietly
//! forgets is worse than no log, so the number is stated on screen.
//! `access_log_retention_days()` is what lets the app say "Kept for 30
//! days" without typing the number itself.
//!
//! `two_engines_access_log.rs` proves what the roll-up files under one
//! entry; it never touches retention, so this file covers that on its own.

mod common;

use std::fs;

use ferry_runtime::{Config, DeviceKind, Engine, EngineListener, PairingState, Root, generate_key};

use common::engines::build_as;

/// A listener that does nothing. This file only checks what is on disk and
/// what the getter reports; it never waits on a callback.
struct Silent;

impl EngineListener for Silent {
    fn devices_changed(&self) {}
    fn transfers_changed(&self) {}
    fn pairing_changed(&self, _state: PairingState) {}
    fn access_log_changed(&self) {}
}

#[test]
fn the_retention_getter_reports_thirty_days() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    assert_eq!(
        mac.engine.access_log_retention_days(),
        30,
        "docs/engine-contract.md item 13 states the log is kept for 30 days"
    );
    mac.engine.stop();
}

#[test]
fn a_day_file_older_than_the_retention_window_is_deleted_at_start() {
    // `access::prune_dir` decides purely from a day file's own eight digit
    // name, never its content, so an empty file dated long ago is enough
    // to prove it is removed. "20200101" is more than thirty days before
    // any date this test could plausibly run on.
    let old_day = "20200101";

    let data = tempfile::tempdir().expect("a temporary data folder");
    let shared = tempfile::tempdir().expect("a temporary shared folder");
    let download = tempfile::tempdir().expect("a temporary download folder");

    let access_log_dir = data.path().join("access_log");
    fs::create_dir_all(&access_log_dir).expect("the access log folder should be makeable");
    fs::write(access_log_dir.join(old_day), []).expect("the old day file should write");

    let config = Config {
        data_dir: data.path().to_string_lossy().into_owned(),
        shared_roots: vec![Root {
            name: "Root".to_owned(),
            path: shared.path().to_string_lossy().into_owned(),
            writable: true,
        }],
        download_dir: download.path().to_string_lossy().into_owned(),
        display_name: "Vamana".to_owned(),
        listen_port: 0,
        key: generate_key().expect("a fresh key pair"),
        kind: DeviceKind::Mac,
    };
    let engine = Engine::new(config, Box::new(Silent)).expect("the engine should build");
    engine.start().expect("the engine should start");

    assert!(
        !access_log_dir.join(old_day).exists(),
        "a day file older than the thirty day retention window must be \
         deleted at start, docs/engine-contract.md item 13"
    );

    engine.stop();
}
