# Security and wire audit

Date: 11 September 2026
Commit: 3cd5451
Scope: pairing and the Noise wire, the loopback WebDAV bridge, the phone's documents provider, network trust, the access log, and the scripts.
Method: read only, no code run, fifteen minutes of wall clock; each finding names a read path, and "plausible" names the step not read.

## Findings

Ranked by cost to the person using Ferry. "Confirmed" means the whole path was read.

| Number | Where | What happens | Effect | Fix |
|---|---|---|---|---|
| 1 (medium, confirmed) | `crates/ferry-core/src/tcp.rs:150-155` and `:474-490`; `crates/ferry-core/src/limits.rs:73` and `:97`; `crates/ferry-runtime/src/engine.rs:2647-2651` | Any device on the trusted Wi-Fi opens eight TCP connections and sends nothing. Each holds a pending slot for the ten second handshake deadline. The accept loop treats the ninth accept as an error and drops it. The attacker repeats every ten seconds. | No phone can pair or connect while the attacker stays on the network. No key is needed. | Add a two second deadline for the first byte, before `negotiate`. Cap pending slots per source address at two. |
| 2 (medium, confirmed) | `crates/ferry-runtime/src/access.rs:25-27`, `:85`, `:634` | A day file holds 10,000 entries. A paired device makes 10,000 cheap `stat` calls in seconds. `append` then returns `Ok(false)` and writes nothing for the rest of the UTC day. | The access log goes blind for the day. The person cannot see what that device did after the fill. | When a day is full, keep an in-memory count per device and write one "log full" entry with the count at the next day boundary. Better, cap entries per device per day, not per day. |
| 3 (medium, confirmed) | `crates/ferry-runtime/src/engine.rs:1405-1445`, `:2894-2900`, and `:3020-3055` | The phone scans a QR code and dials the addresses in it. On a good `IK` handshake the phone stores the peer with no question asked. `trust_current_network` then trusts the network it is on. An attacker who shows the phone their own QR code becomes a paired device. | The attacker reads and writes the phone's shared roots until the person forgets the device in Devices. The phone never showed the attacker's name before storing. | Show `Requested {name}` on the phone too and require the same confirm as the Mac side. `finish_pairing` at `:3020-3055` runs the hello and then `save_peers` with no confirm gate on the scanning side, so the phone's UI cannot intervene. |
| 4 (low, confirmed) | `crates/ferry-runtime/src/engine.rs:2697-2700`, `:2735-2742`, `:2800-2820` | While the Mac is open to code pairing, the first `PairByCode` connection holds the code slot. The real phone gets `PairingBusy` and nothing is reported. Any device on the network can be first, in every two minute window. | The person sees a code that does not match the phone, rejects it, and cannot pair while the attacker keeps arriving. No data is lost. | Same fix as finding 1. Also report `PairingBusy` to the person, so the failure is visible. |
| 5 (low, confirmed) | `crates/ferry-runtime/src/engine.rs:3329`; `crates/ferry-core/src/tcp.rs:27-30` | Serving connections are pushed to `live.serving` with no cap. After the handshake the write timeout is cleared. A paired device opens many connections and never reads. Each holds one thread for ever. | One misbehaving paired phone can exhaust threads on the Mac. Needs a paired key. | Cap serving connections per peer at eight, the way the bridge caps at 32. Keep a write timeout of `IDLE_TIMEOUT_SECS`. |
| 6 (low, confirmed) | `crates/ferry-runtime/src/networks.rs:65-72`; `crates/ferry-runtime/src/engine.rs:1110` | Trust is by Wi-Fi network name only. A network with the same name as home is trusted. | Advertising and accepting resume on a copied network. A stranger learns only the random instance name and protocol version, and gains the surface in findings 1 and 4. No file data. | Record this as a known limit in `docs/protocol.md`. A name match is not proof of the home network. |
| 7 (low, confirmed) | `crates/ferry-runtime/src/dav/server.rs:41`, `:48`, `:155-165` | The bridge allows 32 live connections. A local process opens 32 and sends nothing. Each holds a slot for the 30 second timeout. The process repeats. | Finder cannot reach the mount while that process runs. Loopback only, no password needed for this. | Use a five second timeout before the first request head is read. Count unauthenticated connections separately, with a cap of four. |

## Found safe

- The bridge binds `127.0.0.1` only, at `crates/ferry-runtime/src/dav/mod.rs:139`.
- `Host` is compared exactly and Basic auth is checked before any body byte, at `crates/ferry-runtime/src/dav/server.rs:240-256`. The password is 16 random bytes per start, at `mod.rs:145`. The compare is constant time, at `server.rs:367`.
- `Transfer-Encoding` is refused at `server.rs:262-268`. The head is capped at 64 KiB and 64 headers, at `crates/ferry-runtime/src/dav/http.rs:22-27`. A body is capped at 256 KiB, a `PUT` at 32 GiB streamed to a spool with a 64 GiB cap, at `http.rs:37-51` and `crates/ferry-runtime/src/dav/put.rs:53`.
- Locks are capped at 4,096, sidecars at 4,096 of 64 KiB, the head cache at 1,024, at `dav/lock.rs:23`, `dav/probes.rs:21-26`, `dav/cache.rs:25`. `PROPFIND` with depth infinity is refused at `server.rs:1271-1276`.
- The mount password goes to NetFS in memory, never on a command line or in a URL, at `macos/Ferry/Engine/FinderMount.swift:117-131`. `MountEndpoint`'s `Debug` hides it, at `crates/ferry-runtime/src/lib.rs:386-396`. The mount folder is named by key hex, never by the peer's name, at `FinderMount.swift:88-92`.
- An unpaired device gets a `KK` handshake against a candidate set that always holds a throwaway key, at `crates/ferry-runtime/src/engine.rs:2707-2722`. It cannot tell an empty peer list from one peer. mDNS gives only a random instance name and the protocol version, at `crates/ferry-core/src/discovery.rs:8` and `:233-239`.
- A pairing mode is honoured only while that method is open, at `engine.rs:2697-2705`. The QR nonce is 16 bytes, checked in the handshake, and spent once under the lock, at `engine.rs:2852-2856`. Expiry and the already-paired check run before any dial, at `engine.rs:1422-1430`.
- A paired device cannot leave its roots. `RemotePath` refuses `..`, `.`, empty parts, NUL, backslash, and over 1,024 bytes, at `crates/ferry-core/src/path.rs:103-125`. Roots open through `cap_std`, so a symlink out of a root is refused, at `crates/ferry-core/src/localfs.rs:13-20`. A read-only root refuses every write verb, at `crates/ferry-core/src/roots.rs:258-334`. Nested roots are refused, at `roots.rs:138-146`.
- Every wire decode is capped, at `crates/ferry-core/src/limits.rs:24-65` and `crates/ferry-core/src/noise.rs:68`. A hello name is 1 to 64 bytes with no control bytes, checked both ways, at `crates/ferry-core/src/rpc.rs:167` and `:249-252`.
- The documents provider is exported only to holders of `MANAGE_DOCUMENTS`, a signature permission, at `android/app/src/main/AndroidManifest.xml:97-101`. Another app reaches a Mac file only through a picker grant. A document id is key hex plus a root-relative path that the engine parses again, at `android/app/src/main/kotlin/app/ferry/provider/FerryDocumentsProvider.kt:446-458`. `ReachableService` is not exported.
- The trusted network file has a name cap and a count cap, and is written by rename, at `crates/ferry-runtime/src/networks.rs:10-16` and `:36-44`. The peer store caps at 64 peers, creates private files with `create_new`, and renames into place, at `crates/ferry-core/src/peers.rs:110-113` and `:441-479`.
- `scripts/gate.sh`, `scripts/env.sh`, and `scripts/gen_bindings.sh` read no untrusted input. A grep for positional arguments, `eval`, `curl`, and `sudo` found none.

## Also read, safe

- `crates/ferry-runtime/src/dav/xml.rs` matches property names by string, at `:53` and `:89`. It expands no entities and reads no DOCTYPE. Its input is already capped at 256 KiB.
- `crates/ferry-runtime/src/pool.rs:53` caps outgoing connections per device at four.
- `crates/ferry-runtime/src/guard.rs:23-47` swaps the served roots behind one handle. A live connection reads the new roots on its next call, so `set_roots` takes effect without a reconnect.
- `crates/ferry-runtime/src/dav/delete.rs:10-20` bounds a recursive delete by `MAX_FOLDER_FILES` and `MAX_FOLDER_DEPTH`.

## Not read

`macos/Ferry/Engine/NetworkName.swift` beyond its function list, `crates/ferry-runtime/src/guard.rs`, and `crates/ferry-runtime/src/held.rs`.

## Fix pass, 11 September 2026

Findings 1, 4, 5, and 7 are fixed with one bound each, in
`crates/ferry-core/src/limits.rs` and the bridge, with tests in
`tests/security_bounds.rs`, and the constants are in contract item 16.
Finding 2 is fixed as a cap per device per day; the "log full" entry was
not written, because it needs a new field on the boundary. Finding 3
changed a rule: both sides now confirm a scan pairing by name before
storing the peer, with tests in `tests/item_12_scan_confirm.rs`, and
contract item 12, the IA, and the protocol doc say so. Finding 6 is
recorded in `docs/protocol.md` as a known limit. One follow-on: the
bridge's thirty-two connection test now meets the four connection
unauthenticated cap first, so it no longer guards the larger cap.
