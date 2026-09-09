//! Throwaway probe for order 0.
//!
//! Serves a fake phone filesystem over WebDAV and logs every request macOS
//! makes. The point is to find out how the macOS WebDAV client behaves:
//! whether it lists directories, whether it fetches byte ranges on demand or
//! downloads whole files, whether writing works, and how fast it reads.
//!
//! Run it, then mount it:
//!
//! ```text
//! cargo run --release
//! mkdir -p /tmp/ferry-probe
//! mount_webdav -S -v ferry-probe http://127.0.0.1:8080/ /tmp/ferry-probe
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// Port comes from argv so a fresh URL can defeat the macOS WebDAV cache.
fn port() -> u16 {
    std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(8080)
}
const BIG_FILE_LEN: u64 = 512 * 1024 * 1024;
const SMALL_FILE_LEN: u64 = 2 * 1024 * 1024;
const NOTES_LEN: u64 = 64;

static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);
static BYTES_SERVED: AtomicU64 = AtomicU64::new(0);
static START: Mutex<Option<Instant>> = Mutex::new(None);

/// One entry in the fake tree.
struct Node {
    path: &'static str,
    is_dir: bool,
    len: u64,
}

const TREE: &[Node] = &[
    Node { path: "", is_dir: true, len: 0 },
    Node { path: "DCIM", is_dir: true, len: 0 },
    Node { path: "DCIM/Camera", is_dir: true, len: 0 },
    Node { path: "DCIM/Camera/IMG_0001.jpg", is_dir: false, len: SMALL_FILE_LEN },
    Node { path: "DCIM/Camera/VID_0002.mp4", is_dir: false, len: BIG_FILE_LEN },
    // Control files. macOS probes for these. Serving them may suppress
    // thumbnail fetches, which matters on a phone holding thousands of photos.
    Node { path: ".ql_disablethumbnails", is_dir: false, len: 0 },
    Node { path: ".ql_disablecache", is_dir: false, len: 0 },
    Node { path: "Download", is_dir: true, len: 0 },
    Node { path: "Download/notes.txt", is_dir: false, len: NOTES_LEN },
];

fn find(path: &str) -> Option<&'static Node> {
    TREE.iter().find(|n| n.path == path)
}

fn children(path: &str) -> Vec<&'static Node> {
    TREE.iter()
        .filter(|n| {
            if n.path.is_empty() {
                return false;
            }
            match n.path.rfind('/') {
                Some(i) => &n.path[..i] == path,
                None => path.is_empty(),
            }
        })
        .collect()
}

/// Deterministic content, so a reader can verify any byte range.
fn byte_at(offset: u64) -> u8 {
    u8::try_from(offset % 251).unwrap_or(0)
}

fn log(line: &str) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let stamp = format!("{}.{:03}", now.as_secs(), now.subsec_millis());
    let text = format!("{stamp} {line}\n");
    print!("{text}");
    let _ = std::io::stdout().flush();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("probe.log") {
        let _ = f.write_all(text.as_bytes());
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(input: &str) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn http_date() -> String {
    // A fixed date is enough for a probe. macOS only needs a valid format.
    "Tue, 09 Sep 2026 12:00:00 GMT".to_string()
}

fn propfind_body(target: &str, depth: &str) -> String {
    let mut nodes: Vec<&Node> = Vec::new();
    if let Some(n) = find(target) {
        nodes.push(n);
    }
    if depth != "0" {
        nodes.extend(children(target));
    }

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n",
    );
    for n in nodes {
        let href = if n.is_dir {
            format!("/{}/", percent_encode(n.path)).replace("//", "/")
        } else {
            format!("/{}", percent_encode(n.path))
        };
        let name = n.path.rsplit('/').next().unwrap_or("root");
        let resourcetype =
            if n.is_dir { "<D:collection/>" } else { "" };
        xml.push_str(&format!(
            "<D:response>\
<D:href>{href}</D:href>\
<D:propstat><D:prop>\
<D:displayname>{name}</D:displayname>\
<D:resourcetype>{resourcetype}</D:resourcetype>\
<D:getcontentlength>{}</D:getcontentlength>\
<D:getlastmodified>{}</D:getlastmodified>\
<D:creationdate>2026-09-09T12:00:00Z</D:creationdate>\
<D:getetag>\"{}-{}\"</D:getetag>\
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>\
</D:response>\n",
            n.len,
            http_date(),
            n.path.len(),
            n.len
        ));
    }
    xml.push_str("</D:multistatus>\n");
    xml
}

fn write_head(out: &mut impl Write, status: &str, headers: &[(&str, String)]) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\n");
    head.push_str("Date: ");
    head.push_str(&http_date());
    head.push_str("\r\nServer: ferry-webdav-probe\r\n");
    head.push_str("DAV: 1, 2\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())
}

fn serve_range(
    out: &mut impl Write,
    node: &Node,
    range: Option<(u64, u64)>,
    body: bool,
) -> std::io::Result<()> {
    let (start, end) = range.unwrap_or((0, node.len.saturating_sub(1)));
    let end = end.min(node.len.saturating_sub(1));
    let len = end.saturating_sub(start) + 1;

    let status = if range.is_some() { "206 Partial Content" } else { "200 OK" };
    let mut headers = vec![
        ("Content-Length", len.to_string()),
        ("Content-Type", "application/octet-stream".to_string()),
        ("Accept-Ranges", "bytes".to_string()),
        ("ETag", format!("\"{}-{}\"", node.path.len(), node.len)),
        ("Last-Modified", http_date()),
    ];
    if range.is_some() {
        headers.push(("Content-Range", format!("bytes {start}-{end}/{}", node.len)));
    }
    write_head(out, status, &headers)?;

    if !body {
        return Ok(());
    }

    let mut buf = vec![0u8; 256 * 1024];
    let mut sent = 0u64;
    while sent < len {
        let n = usize::try_from((len - sent).min(buf.len() as u64)).unwrap_or(0);
        for (i, slot) in buf[..n].iter_mut().enumerate() {
            *slot = byte_at(start + sent + i as u64);
        }
        out.write_all(&buf[..n])?;
        sent += n as u64;
    }
    BYTES_SERVED.fetch_add(len, Ordering::Relaxed);
    Ok(())
}

fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        let suffix: u64 = b.trim().parse().ok()?;
        return Some((total.saturating_sub(suffix), total.saturating_sub(1)));
    }
    let start: u64 = a.trim().parse().ok()?;
    let end = if b.trim().is_empty() { total.saturating_sub(1) } else { b.trim().parse().ok()? };
    Some((start, end))
}

fn handle(stream: TcpStream) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut reader = BufReader::new(stream.try_clone()?);

    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let request_line = request_line.trim_end().to_string();
        if request_line.is_empty() {
            continue;
        }

        let mut headers: HashMap<String, String> = HashMap::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }

        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let raw_target = parts.next().unwrap_or("/").to_string();

        let body_len: usize =
            headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        let mut body = vec![0u8; body_len];
        if body_len > 0 {
            reader.read_exact(&mut body)?;
        }

        let target = percent_decode(raw_target.trim_start_matches('/').trim_end_matches('/'));
        let depth = headers.get("depth").cloned().unwrap_or_else(|| "infinity".into());
        let range_header = headers.get("range").cloned();

        let n = REQUEST_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            *START.lock().unwrap() = Some(Instant::now());
        }
        log(&format!(
            "#{n} {peer} {method} {raw_target} depth={depth} range={} ua={}",
            range_header.clone().unwrap_or_else(|| "-".into()),
            headers.get("user-agent").cloned().unwrap_or_else(|| "-".into())
        ));

        let mut out = stream.try_clone()?;
        match method.as_str() {
            "OPTIONS" => {
                write_head(&mut out, "200 OK", &[
                    ("Allow",
                     "OPTIONS,GET,HEAD,PUT,DELETE,PROPFIND,PROPPATCH,MKCOL,COPY,MOVE,LOCK,UNLOCK"
                        .to_string()),
                    ("MS-Author-Via", "DAV".to_string()),
                    ("Content-Length", "0".to_string()),
                ])?;
            }
            "PROPFIND" => {
                if find(&target).is_none() {
                    write_head(&mut out, "404 Not Found", &[("Content-Length", "0".into())])?;
                } else {
                    let xml = propfind_body(&target, &depth);
                    write_head(&mut out, "207 Multi-Status", &[
                        ("Content-Type", "application/xml; charset=\"utf-8\"".to_string()),
                        ("Content-Length", xml.len().to_string()),
                    ])?;
                    out.write_all(xml.as_bytes())?;
                }
            }
            "GET" | "HEAD" => match find(&target) {
                Some(node) if !node.is_dir => {
                    let range = range_header.as_deref().and_then(|v| parse_range(v, node.len));
                    serve_range(&mut out, node, range, method == "GET")?;
                }
                _ => write_head(&mut out, "404 Not Found", &[("Content-Length", "0".into())])?,
            },
            "LOCK" => {
                let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
<D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope>\
<D:depth>infinity</D:depth><D:timeout>Second-3600</D:timeout>\
<D:locktoken><D:href>opaquelocktoken:ferry-probe-token</D:href></D:locktoken>\
</D:activelock></D:lockdiscovery></D:prop>\n";
                write_head(&mut out, "200 OK", &[
                    ("Content-Type", "application/xml; charset=\"utf-8\"".to_string()),
                    ("Lock-Token", "<opaquelocktoken:ferry-probe-token>".to_string()),
                    ("Content-Length", xml.len().to_string()),
                ])?;
                out.write_all(xml.as_bytes())?;
            }
            "UNLOCK" => write_head(&mut out, "204 No Content", &[("Content-Length", "0".into())])?,
            "PUT" => {
                log(&format!("    PUT accepted {} bytes for {target}", body.len()));
                write_head(&mut out, "201 Created", &[("Content-Length", "0".into())])?;
            }
            "MKCOL" => write_head(&mut out, "201 Created", &[("Content-Length", "0".into())])?,
            "DELETE" | "MOVE" | "COPY" | "PROPPATCH" => {
                write_head(&mut out, "204 No Content", &[("Content-Length", "0".into())])?;
            }
            _ => write_head(&mut out, "405 Method Not Allowed", &[("Content-Length", "0".into())])?,
        }
        out.flush()?;
    }
}

fn main() -> std::io::Result<()> {
    let _ = std::fs::remove_file("probe.log");
    let port = port();
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    log(&format!("probe listening on http://127.0.0.1:{port}/"));
    log("tree: DCIM/Camera/IMG_0001.jpg (2 MiB), DCIM/Camera/VID_0002.mp4 (512 MiB), Download/notes.txt");

    std::thread::spawn(|| {
        let mut last = 0u64;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let now = BYTES_SERVED.load(Ordering::Relaxed);
            if now != last {
                let elapsed = START.lock().unwrap().map(|s| s.elapsed().as_secs_f64()).unwrap_or(1.0);
                #[allow(clippy::cast_precision_loss)]
                let mb = now as f64 / 1_048_576.0;
                log(&format!("    served {mb:.1} MiB total, {:.1} MiB/s average", mb / elapsed));
                last = now;
            }
        }
    });

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                std::thread::spawn(move || {
                    if let Err(e) = handle(s) {
                        log(&format!("    connection ended: {e}"));
                    }
                });
            }
            Err(e) => log(&format!("accept failed: {e}")),
        }
    }
    Ok(())
}
