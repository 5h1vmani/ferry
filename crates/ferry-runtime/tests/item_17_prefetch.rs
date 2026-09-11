//! Thumbnail prefetch, `docs/engine-contract.md`, item 17.
//!
//! Two paired engines in one process, the same way `tests/dav.rs` runs
//! them: the Mac mounts the phone's shared root and browses it through the
//! bridge, and the phone serves real files from a real folder. Both tests
//! read the phone's own access log, so every claim about what did or did
//! not reach the wire is checked against what the phone was actually
//! asked, not against a mock.
//!
//! The extension check and the head cache's first in first out bound are
//! pure and have their own tests inside `src/dav/heads.rs`, since the cache
//! and the constants item 17 names are `pub(crate)` and no integration test
//! can reach them.

mod common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use ferry_runtime::{AccessEntry, AccessVerb, Actor, DeviceKind, generate_key};

use common::{PATIENCE, Side, TestClient, build_side, count_entries, loopback_addr, port_of};

/// The Mac's own log entry for one prefetched listing, if it has been
/// written: actor `This`, verb `Read`, and the folder's path.
///
/// Item 17: "The prefetch of one listing is one `Read` entry on this side
/// through `record_this`: the folder path, the total bytes read, and
/// `files` set to the count."
fn prefetch_entry(log: &[AccessEntry], folder: &str) -> Option<AccessEntry> {
    log.iter()
        .find(|entry| {
            entry.actor == Actor::This
                && entry.verb == AccessVerb::Read
                && entry.path == folder
                && entry.files.is_some()
        })
        .cloned()
}

/// Waits until `ready` answers true, or fails the test after [`PATIENCE`].
fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The Mac and the phone, paired, with the phone's bridge mounted and a
/// connected HTTP client. `files` seeds the phone's one shared root, named
/// `Root`.
struct Mounted {
    mac: Side,
    phone: Side,
    client: TestClient,
    host: String,
    user: String,
    password: String,
}

impl Mounted {
    fn new(files: &[(&str, &[u8])]) -> Self {
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
            phone_key,
            files,
            &mac_key,
            "Vamana",
            DeviceKind::Mac,
        );
        // Only a reachable device accepts inbound connections at all.
        phone.engine.set_reachable(true);

        let phone_key_hex = mac
            .engine
            .devices()
            .first()
            .expect("the phone should already be paired, from the seeded peer store")
            .key_hex
            .clone();
        // Discovery is not running in a test, so this `list` is what gives
        // the Mac the phone's address and marks it reachable.
        mac.engine.offer_candidate(loopback_addr(&phone));
        mac.engine
            .list(phone_key_hex.clone(), String::new())
            .expect("listing the phone's root should succeed once dialable");
        let endpoint = mac
            .engine
            .mount_start(phone_key_hex)
            .expect("mount_start should succeed for a paired, reachable device");

        let port = port_of(&endpoint.url);
        let addr: SocketAddr = format!("127.0.0.1:{port}")
            .parse()
            .expect("a loopback address");
        Self {
            client: TestClient::connect(addr),
            host: format!("127.0.0.1:{port}"),
            user: endpoint.user,
            password: endpoint.password,
            mac,
            phone,
        }
    }

    /// One `PROPFIND` at `depth`, which must answer 207.
    fn propfind(&mut self, path: &str, depth: &str) {
        let host = self.host.clone();
        let auth = (self.user.clone(), self.password.clone());
        let response = self.client.request(
            "PROPFIND",
            path,
            &host,
            Some((auth.0.as_str(), auth.1.as_str())),
            &[("Depth", depth.to_owned())],
            Some(b""),
        );
        assert_eq!(response.status, 207, "PROPFIND {path} at depth {depth}");
    }

    /// One `GET` of `[first, last]`, which must answer 206.
    fn ranged_get(&mut self, path: &str, first: u64, last: u64) -> Vec<u8> {
        let host = self.host.clone();
        let auth = (self.user.clone(), self.password.clone());
        let response = self.client.request(
            "GET",
            path,
            &host,
            Some((auth.0.as_str(), auth.1.as_str())),
            &[("Range", format!("bytes={first}-{last}"))],
            None,
        );
        assert_eq!(response.status, 206, "GET {path} with a range");
        response.body
    }

    /// One `GET` with no `Range`, which must answer 200 with the whole
    /// file.
    fn whole_get(&mut self, path: &str) -> Vec<u8> {
        let host = self.host.clone();
        let auth = (self.user.clone(), self.password.clone());
        let response = self.client.request(
            "GET",
            path,
            &host,
            Some((auth.0.as_str(), auth.1.as_str())),
            &[],
            None,
        );
        assert_eq!(response.status, 200, "GET {path} with no range");
        response.body
    }

    /// How many reads the phone has served and finalised so far.
    fn peer_reads(&self) -> usize {
        count_entries(
            &self.phone.engine.access_log(None, 1000),
            Actor::Peer,
            AccessVerb::Read,
        )
    }

    /// How many operations of `verb` on exactly `path` the phone has
    /// served and finalised so far.
    fn served(&self, verb: AccessVerb, path: &str) -> usize {
        self.phone
            .engine
            .access_log(None, 1000)
            .iter()
            .filter(|entry| entry.actor == Actor::Peer && entry.verb == verb && entry.path == path)
            .count()
    }

    /// The Mac's own entry for the prefetch of `folder`, once written.
    fn prefetch_of(&self, folder: &str) -> Option<AccessEntry> {
        prefetch_entry(&self.mac.engine.access_log(None, 1000), folder)
    }
}

#[test]
// Item 17: "One test lists a folder holding three images and one text file
// over PROPFIND, waits until the phone's access log shows three reads, then
// requests the first kilobyte of one image with a `Range` header and proves
// the phone's log gains no new read."
fn a_listing_prefetches_its_image_heads_and_a_thumbnail_then_costs_no_read() {
    let a = vec![0xA1u8; 4_000];
    let b = vec![0xB2u8; 5_000];
    let c = vec![0xC3u8; 6_000];
    let mut it = Mounted::new(&[
        ("Photos/a.jpg", a.as_slice()),
        ("Photos/b.JPEG", b.as_slice()),
        ("Photos/c.png", c.as_slice()),
        ("Photos/Notes.txt", b"not an image".as_slice()),
    ]);

    let reads_before = it.peer_reads();
    it.propfind("/Root/Photos", "1");

    // The Mac writes its own entry once the whole listing is prefetched,
    // so this is the cheap signal that the three reads have happened.
    wait_until("the Mac records the prefetch of Root/Photos", || {
        it.prefetch_of("Root/Photos").is_some()
    });
    let entry = it
        .prefetch_of("Root/Photos")
        .expect("the entry was just seen");
    assert_eq!(
        entry.files,
        Some(3),
        "the three images are prefetched and the text file is not"
    );
    assert_eq!(
        entry.bytes,
        Some(4_000 + 5_000 + 6_000),
        "each image is smaller than HEAD_LEN, so the whole file is its head"
    );

    // The phone finalises a served entry when its connection moves to a
    // different path (item 13), so the last of the three reads is still
    // pending. This stat is what moves it on, rather than waiting five
    // seconds for the idle rule.
    it.propfind("/Root/Photos/Notes.txt", "0");
    wait_until("the phone has logged all three reads", || {
        it.peer_reads() == reads_before + 3
    });

    // Past the listing cache's two second TTL, so the `PROPFIND` below is
    // certain to list on the wire. That makes what the phone has pending
    // on this connection known, which the stat count at the end needs.
    std::thread::sleep(Duration::from_millis(2_100));
    it.propfind("/Root/Photos", "1");

    let body = it.ranged_get("/Root/Photos/a.jpg", 0, 1_023);
    assert_eq!(
        body,
        a[..1_024],
        "the head must carry the file's real bytes"
    );

    // Whatever the `GET` had just made the phone serve would still be
    // pending, and a pending entry is not in the log yet. This stat moves
    // the connection to another path, which finalises anything the `GET`
    // left behind, so the counts below hold everything that happened.
    it.propfind("/Root/Photos/Notes.txt", "0");
    assert_eq!(
        it.served(AccessVerb::Read, "Root/Photos/a.jpg"),
        1,
        "only the prefetch read this file: the thumbnail request cost no read"
    );
    assert_eq!(
        it.served(AccessVerb::Stat, "Root/Photos/a.jpg"),
        0,
        "the GET took the file's size and time from the listing cache, not the wire"
    );

    it.mac.engine.stop();
    it.phone.engine.stop();
}

#[test]
// Item 17: "A second test proves the head cache misses after the file is
// written again with a new size." The head is keyed by the size and time
// the listing reported, so a replaced file misses on its own.
fn a_file_written_again_with_a_new_size_misses_its_cached_head() {
    let first = vec![0x11u8; 2_000];
    let second = vec![0x22u8; 4_000];
    let mut it = Mounted::new(&[("Photos/a.jpg", first.as_slice())]);

    it.propfind("/Root/Photos", "1");
    wait_until("the Mac records the prefetch of Root/Photos", || {
        it.prefetch_of("Root/Photos").is_some()
    });
    let entry = it
        .prefetch_of("Root/Photos")
        .expect("the entry was just seen");
    assert_eq!(entry.files, Some(1), "the one image is prefetched");

    // The phone's own file changes under the bridge, the way a person
    // saving over a photo changes it.
    let on_disk = it.phone.shared.path().join("Root/Photos/a.jpg");
    std::fs::write(&on_disk, &second).expect("the replacement should write");

    // The listing cache holds the old size for two seconds. Past that, the
    // next PROPFIND lists the folder again and reports the new size.
    std::thread::sleep(Duration::from_millis(2_100));
    it.propfind("/Root/Photos", "1");

    let body = it.ranged_get("/Root/Photos/a.jpg", 0, 1_023);
    assert_eq!(
        body,
        second[..1_024],
        "a new size is a new key, so the stale head must not be served"
    );

    it.mac.engine.stop();
    it.phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Audit `docs/audits/third-run-engine.md`, finding 4: one body, two versions.
// ---------------------------------------------------------------------------

/// A `GET` that needs a byte the cached head does not hold must stat on the
/// wire first, and may use the head only when the fresh size and time match
/// the head's key.
///
/// Before this, the listing cache supplied the size and the head supplied
/// its first 65536 bytes, while the rest was read live from a file that had
/// since changed. Finder received one file made of two versions, with the
/// declared length met, so nothing reported an error.
#[test]
fn a_get_that_needs_the_wire_never_mixes_two_versions_of_a_file() {
    // Larger than HEAD_LEN, which is 64 KiB, so the whole body cannot come
    // from the head and the wire is needed for the rest.
    let first = vec![0x11u8; 100_000];
    let second = vec![0x22u8; 120_000];
    let mut it = Mounted::new(&[("Photos/a.jpg", first.as_slice())]);

    it.propfind("/Root/Photos", "1");
    wait_until("the Mac records the prefetch of Root/Photos", || {
        it.prefetch_of("Root/Photos").is_some()
    });
    let entry = it
        .prefetch_of("Root/Photos")
        .expect("the entry was just seen");
    assert_eq!(
        entry.bytes,
        Some(65_536),
        "the head holds the first HEAD_LEN bytes of the file, not all of it"
    );

    // The phone's own file changes under the bridge, inside the listing
    // cache's two second window, the way a person saving over a photo
    // changes it.
    let on_disk = it.phone.shared.path().join("Root/Photos/a.jpg");
    std::fs::write(&on_disk, &second).expect("the replacement should write");

    let body = it.whole_get("/Root/Photos/a.jpg");
    assert_eq!(
        body.len(),
        second.len(),
        "the length must come from a fresh stat, not from the stale listing"
    );
    assert_eq!(
        body, second,
        "every byte must come from one version of the file"
    );

    it.mac.engine.stop();
    it.phone.engine.stop();
}
