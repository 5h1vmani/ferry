# Order 0 spike: findings

Date run: 9 September 2026.
Machine: macOS 26.6.2, build 25G83, Apple silicon.
macOS WebDAV client version seen on the wire: `WebDAVFS/3.0.0 Darwin/25.6.0`.

The probe is in `spike/webdav-probe`. It serves a fake phone filesystem over
WebDAV and logs every request macOS makes.

## Question 1: can macOS mount a WebDAV server and browse it in Finder?

**Yes.** This is the important result.

The mount needed no `sudo`, no TLS, no entitlement, and no Apple Developer
Program membership.

```
mount_webdav -v ferry-probe http://127.0.0.1:8080/ /tmp/ferry-probe
```

Finder listed the tree, walked into subfolders, and showed correct file sizes.

This means Ferry can give a Mac user a browsable phone without a File Provider
extension and without paying Apple. The File Provider route becomes a polish
step, not a prerequisite.

## Question 2: does macOS fetch byte ranges, or download whole files?

**It fetches ranges, but not the way a server would prefer.**

Opening a folder with two media files produced ten content requests. Every one
used an open-ended range, and macOS aborted each stream early. The server saw
`Broken pipe` each time.

```
GET /DCIM/Camera/VID_0002.mp4  range=-
GET /DCIM/Camera/VID_0002.mp4  range=bytes=147456-
GET /DCIM/Camera/VID_0002.mp4  range=bytes=409600-
GET /DCIM/Camera/VID_0002.mp4  range=bytes=671744-
GET /DCIM/Camera/VID_0002.mp4  range=bytes=2441216-
```

Good news: a 512 MiB file was never downloaded in full. Finder read a few
hundred kilobytes and stopped.

Bad news: an open-ended range means "send the rest of the file". A naive server
starts streaming 512 MiB and then takes a broken pipe. Over a phone link that
wastes the whole connection.

**Design requirement.** When a range has no end, serve a bounded amount and
stop. Detect client disconnect and abandon the read immediately.

## Question 3: how chatty is the macOS WebDAV client?

Very chatty. Opening one folder holding two files, on a cold mount:

| Measure | Count |
|---|---|
| Total requests | 58 |
| `PROPFIND` | most of them |
| `GET` for file content | 10 |
| `PUT` written by Finder | 4 |
| Requests for Apple metadata paths that do not exist | 32 |

The 32 wasted requests are probes for `._name` sidecars, `.DS_Store`,
`.hidden`, `.Spotlight-V100`, and several `.metadata_*` files.

On localhost this took 0.79 seconds. Localhost has no round-trip cost. A phone
on Wi-Fi has one. At 5 ms per round trip, 58 requests cost about 0.3 seconds of
pure waiting, and a folder with hundreds of photos costs far more.

**Design requirement.** The Mac side answers Apple's metadata probes locally
and never puts them on the wire. It also caches directory listings.

A second visit to the same folder cost 17 requests instead of 58, so macOS does
cache. The cost is front-loaded onto the first visit.

## Question 4: does Finder write to the volume?

**Yes, immediately and without asking.**

The moment Finder opened `DCIM`, it wrote `.DS_Store` and `._.DS_Store` into
it, using `PUT` wrapped in `LOCK` and `UNLOCK`.

Writing Apple metadata files into a phone's camera folder is not acceptable.

**Design requirements.** Ferry swallows writes to `.DS_Store` and `._*` paths
and returns success without storing anything. Ferry must also implement `LOCK`
and `UNLOCK`, because macOS uses them before writing. The probe advertised
`DAV: 1, 2` and returned a fixed lock token, and macOS accepted that.

## Question 5: can thumbnail fetches be suppressed?

**Not answered.** Serving `.ql_disablethumbnails` and `.ql_disablecache` did
not stop the content fetches. In the cold run macOS never asked for either file
by its plain path, so the lever was never consulted. Ten content requests
happened anyway.

This matters at scale. Two media files caused ten content requests. A real
camera folder holds hundreds. Finding a way to suppress or throttle thumbnail
generation is open work for phase 2.

## Question 6: throughput

**Not answered.** The probe server itself serves 512 MiB over localhost at
about 1.5 GB/s, so the server is not a bottleneck. Any number measured later
belongs to the macOS client.

Measuring the client needs a bulk read through the mount, and that is blocked.
See the blocked section below.

## Question 7: File Provider entitlements on a free Apple personal team

**Blocked.** No Apple ID is signed into Xcode on this machine.

```
security find-identity -v -p codesigning   ->  0 valid identities found
~/Library/Developer/Xcode/UserData/Provisioning Profiles  ->  does not exist
```

Xcode cannot provision an App Group or a File Provider entitlement without a
team, free or paid. This needs a person to sign in.

## Question 8: can File Provider read byte ranges, or only whole files?

**It can read bounded byte ranges.** This was read out of the macOS 26.5 SDK
headers, so it needs no signing and no Apple ID.

The `NSFileProviderPartialContentFetching` protocol carries this method:

```
fetchPartialContents(for:version:request:minimalRange:aligningTo:options:
                     completionHandler:)
```

Available since macOS 12.3, on macOS only.

The system passes a bounded range and an alignment value, which is always a
power of two. The extension may return any aligned range that covers the
request. The system also passes an `NSProgress`, and it cancels that progress
when the user stops reading. The extension is expected to stop fetching.

The system invokes this method instead of the whole-file
`fetchContentsForItemWithIdentifier` whenever a partial range is requested.

This is a better contract than WebDAV gives. WebDAV sent open-ended ranges and
signalled cancellation by breaking the connection. File Provider states the
range up front and cancels explicitly.

## The two Finder routes, compared

| | WebDAV bridge | File Provider extension |
|---|---|---|
| Works today on this Mac | Yes, proven above | Unknown, blocked on Xcode sign-in |
| Needs an Apple entitlement | No | Yes |
| May need the paid Apple program | No | Unknown |
| Byte ranges | Open-ended, aborted by broken pipe | Bounded, with explicit cancellation |
| Cost of one folder open | 58 requests on a cold open | System keeps a replicated item database |
| Apple metadata probes | 32 of the 58 requests | Answered by the system, not by the phone |
| Finder writes `.DS_Store` | Yes, over the wire | Yes, but through a call the extension can reject |
| Build effort | Days | Weeks |

Recommendation: build the WebDAV bridge first, because it is proven and cheap.
Treat the File Provider extension as the better long-term answer, and decide it
once the entitlement question is settled.

## Blocked, and why

Two items need a person at the keyboard.

**Bulk read through the mount.** macOS blocks this shell from reading files on
a network volume. `stat` reached the server, but `ls` and `dd` were refused
with `Operation not permitted`, and no request arrived at the server. Routing
through Finder with `osascript` also failed, with
`Not authorized to send Apple events to Finder`.

Finder itself has the access, which is why `open` worked and produced all the
data above.

**Xcode sign-in.** Needs an Apple ID and a password.

## What this changes in the plan

1. The Finder mount no longer depends on File Provider. A WebDAV bridge is days
   of work, not weeks. Phase 2 should start there. File Provider stays on the
   list as the better long-term route, not as a prerequisite.
2. The hard part of the Finder feature is not bandwidth. It is chatter,
   Finder's metadata writes, and thumbnail storms. All three are solvable on
   the Mac side, and all three are now specified above.
3. The file operations layer needs bounded reads and cancellation from version
   one. That was already the design. This spike confirms why.
