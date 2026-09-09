# Spikes

Throwaway probes. Each one answers a single question and is then kept only as
evidence. None of this code becomes part of Ferry.

| Probe | Question | Result |
|---|---|---|
| `webdav-probe` | How does the macOS WebDAV client behave against a server? | Answered. See `docs/spike-0-findings.md`. |

## webdav-probe

Serves a fake phone filesystem over WebDAV and logs every request macOS makes.

```bash
cd spike/webdav-probe
cargo run --release -- 8080
```

Then mount it in another shell:

```bash
mkdir -p /tmp/ferry-probe
mount_webdav -v ferry-probe http://127.0.0.1:8080/ /tmp/ferry-probe
open /tmp/ferry-probe/DCIM/Camera
```

Requests are logged to the terminal and to `probe.log`.

Two notes from running it.

macOS caches lookups per URL. To repeat a cold-start test, use a different
port so the URL is new.

macOS blocks a plain shell process from reading files on a network volume. The
error is `Operation not permitted`, and no request reaches the server. Finder
has that access, so drive the test with `open` rather than `ls` or `cat`.
