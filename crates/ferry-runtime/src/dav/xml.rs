//! `PROPFIND` request property reading, and the `multistatus` reply.
//!
//! Both directions are hand-written, per `docs/engine-contract.md`, item 6.
//! The reader is tolerant on purpose: Finder's own request bodies vary by
//! macOS version and by which properties it already trusts from an earlier
//! answer, and a strict parser would have to be rewritten for every shape
//! seen. The writer is minimal: six properties, one namespace, no attempt
//! at general XML.

use std::fmt::Write as _;

use crate::dav::http::{etag, iso8601, rfc1123};

/// Which of the six properties a `PROPFIND` asked for.
///
/// An empty body, an `<allprop/>` body, or a body this reader does not
/// recognise as a `<prop>` request all mean "every property": `WebDAV`'s own
/// default, and the honest fallback when a request cannot be read exactly.
///
/// Six flags naming six fixed `WebDAV` property names, not a state machine:
/// clippy's usual worry about a bool pile does not apply to a set that is
/// only ever read one field at a time by name.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct PropSet {
    pub(crate) resourcetype: bool,
    pub(crate) getcontentlength: bool,
    pub(crate) getlastmodified: bool,
    pub(crate) getetag: bool,
    pub(crate) creationdate: bool,
    pub(crate) displayname: bool,
}

impl PropSet {
    fn all() -> Self {
        Self {
            resourcetype: true,
            getcontentlength: true,
            getlastmodified: true,
            getetag: true,
            creationdate: true,
            displayname: true,
        }
    }
}

/// Reads a `PROPFIND` body for the properties it names.
///
/// Not a real XML parser: this looks for each of the six known property
/// element names as a substring of the body text, ignoring namespace
/// prefixes and whitespace. A `<propname/>` or `<allprop/>` request, or any
/// body this reader cannot find a `<prop` block in, is answered as "every
/// property".
pub(crate) fn requested_props(body: &[u8]) -> PropSet {
    if body.is_empty() {
        return PropSet::all();
    }
    let text = String::from_utf8_lossy(body);
    if !text.contains("prop") || text.contains("allprop") {
        return PropSet::all();
    }
    PropSet {
        resourcetype: text.contains("resourcetype"),
        getcontentlength: text.contains("getcontentlength"),
        getlastmodified: text.contains("getlastmodified"),
        getetag: text.contains("getetag"),
        creationdate: text.contains("creationdate"),
        displayname: text.contains("displayname"),
    }
}

/// One `PROPPATCH` request body, tolerantly read.
pub(crate) struct Proppatch {
    /// The local name (namespace prefix stripped) of every property inside
    /// the request's `<D:set><D:prop>` block, in the order they appear.
    pub(crate) names: Vec<String>,
    /// The text the first of `getlastmodified` or `Win32LastModifiedTime`
    /// carried, whichever appears. Both use the `rfc1123` shape
    /// `http::parse_rfc1123` reads.
    pub(crate) mtime_text: Option<String>,
}

/// Reads a `PROPPATCH` body for the properties it tries to set.
///
/// Not a real XML parser: this walks bare `<tag>` tokens with a small
/// amount of state (inside `<set>`? inside `<prop>`? which leaf element is
/// currently open?), the same tolerant approach `requested_props` takes
/// for a `PROPFIND` body. A `<remove>` block is not read: I2 only ever
/// sets the modified time, never removes a property.
pub(crate) fn read_proppatch(body: &[u8]) -> Proppatch {
    let text = String::from_utf8_lossy(body);
    let mut names = Vec::new();
    let mut mtime_text = None;
    let mut in_set = false;
    let mut in_prop = false;
    let mut leaf: Option<String> = None;
    let mut leaf_text = String::new();

    for chunk in text.split('<').skip(1) {
        let Some(tag_end) = chunk.find('>') else {
            continue;
        };
        let inner = &chunk[..tag_end];
        let following = &chunk[tag_end + 1..];
        let closing = inner.starts_with('/');
        let self_closing = inner.trim_end().ends_with('/');
        let body_part = inner.trim_start_matches('/').trim_end_matches('/').trim();
        let raw_name = body_part.split_whitespace().next().unwrap_or("");
        let local = raw_name.rsplit(':').next().unwrap_or(raw_name).to_owned();
        let local_lower = local.to_ascii_lowercase();

        match local_lower.as_str() {
            "" => {}
            "set" => in_set = !closing && !self_closing,
            "remove" => {
                if !closing {
                    in_set = false;
                }
            }
            "prop" if in_set || closing => {
                if closing {
                    in_prop = false;
                } else if in_set {
                    in_prop = !self_closing;
                }
            }
            _ if in_prop && !closing => {
                names.push(local.clone());
                leaf = Some(local_lower.clone());
                leaf_text.clear();
                if self_closing {
                    leaf = None;
                }
            }
            _ if closing => {
                if leaf.as_deref() == Some(local_lower.as_str())
                    && mtime_text.is_none()
                    && (local_lower == "getlastmodified" || local_lower == "win32lastmodifiedtime")
                {
                    mtime_text = Some(leaf_text.trim().to_owned());
                }
                leaf = None;
            }
            _ => {}
        }

        if leaf.is_some() {
            leaf_text.push_str(following);
        }
    }

    Proppatch { names, mtime_text }
}

/// The `multistatus` body for one `PROPPATCH` answer: one `href`, and a
/// `propstat` for the properties this bridge accepted (200) and one for
/// every other property it was asked to set (403).
/// `docs/engine-contract.md`, item 6, I2.
pub(crate) fn proppatch_multistatus(
    path: &str,
    is_dir: bool,
    accepted: &[String],
    refused: &[String],
) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n<D:response><D:href>",
    );
    xml.push_str(&href_for(path, is_dir));
    xml.push_str("</D:href>\n");
    propstat(&mut xml, accepted, "200 OK");
    propstat(&mut xml, refused, "403 Forbidden");
    xml.push_str("</D:response>\n</D:multistatus>\n");
    xml
}

fn propstat(xml: &mut String, names: &[String], status: &str) {
    if names.is_empty() {
        return;
    }
    xml.push_str("<D:propstat><D:prop>");
    for name in names {
        let _ = write!(xml, "<D:{}/>", escape(name));
    }
    let _ = writeln!(
        xml,
        "</D:prop><D:status>HTTP/1.1 {status}</D:status></D:propstat>"
    );
}

/// One file or directory to render as a `<D:response>`.
pub(crate) struct Item<'a> {
    /// Root-relative, no leading slash. Empty for the mount root.
    pub(crate) path: &'a str,
    /// The last path segment, for `displayname`. Empty for the mount root.
    pub(crate) name: &'a str,
    pub(crate) is_dir: bool,
    pub(crate) size: u64,
    pub(crate) modified_unix_secs: i64,
}

/// The whole `multistatus` body for one `PROPFIND` answer.
pub(crate) fn multistatus(items: &[Item<'_>], props: &PropSet) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n",
    );
    for item in items {
        xml.push_str(&response(item, props));
    }
    xml.push_str("</D:multistatus>\n");
    xml
}

fn response(item: &Item<'_>, props: &PropSet) -> String {
    let href = href_for(item.path, item.is_dir);
    let mut prop_xml = String::new();
    if props.displayname {
        prop_xml.push_str("<D:displayname>");
        prop_xml.push_str(&escape(item.name));
        prop_xml.push_str("</D:displayname>");
    }
    if props.resourcetype {
        prop_xml.push_str(if item.is_dir {
            "<D:resourcetype><D:collection/></D:resourcetype>"
        } else {
            "<D:resourcetype/>"
        });
    }
    if props.getcontentlength {
        let _ = write!(
            prop_xml,
            "<D:getcontentlength>{}</D:getcontentlength>",
            item.size
        );
    }
    if props.getlastmodified {
        let _ = write!(
            prop_xml,
            "<D:getlastmodified>{}</D:getlastmodified>",
            rfc1123(item.modified_unix_secs)
        );
    }
    if props.getetag {
        let _ = write!(
            prop_xml,
            "<D:getetag>{}</D:getetag>",
            escape(&etag(item.size, item.modified_unix_secs))
        );
    }
    if props.creationdate {
        // `creationdate` equals the modified time: the peer's file
        // operations layer tracks no separate creation time, and
        // `docs/engine-contract.md`, item 6, says to state that here
        // rather than guess one.
        let _ = write!(
            prop_xml,
            "<D:creationdate>{}</D:creationdate>",
            iso8601(item.modified_unix_secs)
        );
    }
    format!(
        "<D:response><D:href>{href}</D:href><D:propstat><D:prop>{prop_xml}</D:prop>\
<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\n"
    )
}

/// The href for one item: a leading slash, the path percent-encoded, and a
/// trailing slash for a directory. The root is `"/"`.
fn href_for(path: &str, is_dir: bool) -> String {
    if path.is_empty() {
        return "/".to_owned();
    }
    let encoded = percent_encode(path);
    if is_dir {
        format!("/{encoded}/")
    } else {
        format!("/{encoded}")
    }
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Escapes text for an XML element body. The five predefined entities are
/// the whole rule; nothing here ever writes an attribute value.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Item, multistatus, proppatch_multistatus, read_proppatch, requested_props};

    #[test]
    fn an_empty_body_asks_for_everything() {
        let props = requested_props(b"");
        assert!(props.displayname && props.getetag && props.creationdate);
    }

    #[test]
    fn an_allprop_body_asks_for_everything() {
        let props = requested_props(b"<D:propfind xmlns:D=\"DAV:\"><D:allprop/></D:propfind>");
        assert!(props.resourcetype && props.getcontentlength);
    }

    #[test]
    fn a_named_prop_list_asks_for_only_its_names() {
        let props = requested_props(
            b"<D:propfind xmlns:D=\"DAV:\"><D:prop><D:getetag/></D:prop></D:propfind>",
        );
        assert!(props.getetag);
        assert!(!props.displayname);
        assert!(!props.getcontentlength);
    }

    #[test]
    fn the_root_hrefs_as_a_single_slash() {
        let items = [Item {
            path: "",
            name: "",
            is_dir: true,
            size: 0,
            modified_unix_secs: 0,
        }];
        let xml = multistatus(&items, &requested_props(b""));
        assert!(xml.contains("<D:href>/</D:href>"), "{xml}");
    }

    #[test]
    fn a_folder_hrefs_with_a_trailing_slash_and_a_file_does_not() {
        let items = [
            Item {
                path: "Desktop",
                name: "Desktop",
                is_dir: true,
                size: 0,
                modified_unix_secs: 0,
            },
            Item {
                path: "Desktop/Q3 notes.md",
                name: "Q3 notes.md",
                is_dir: false,
                size: 12,
                modified_unix_secs: 0,
            },
        ];
        let xml = multistatus(&items, &requested_props(b""));
        assert!(xml.contains("<D:href>/Desktop/</D:href>"), "{xml}");
        assert!(
            xml.contains("<D:href>/Desktop/Q3%20notes.md</D:href>"),
            "{xml}"
        );
    }

    #[test]
    fn a_name_with_xml_special_characters_is_escaped() {
        let items = [Item {
            path: "a&b",
            name: "a&b",
            is_dir: false,
            size: 0,
            modified_unix_secs: 0,
        }];
        let xml = multistatus(&items, &requested_props(b""));
        assert!(
            xml.contains("<D:displayname>a&amp;b</D:displayname>"),
            "{xml}"
        );
    }

    #[test]
    fn read_proppatch_reads_getlastmodified_and_its_date() {
        let body = b"<?xml version=\"1.0\"?><D:propertyupdate xmlns:D=\"DAV:\"><D:set><D:prop>\
<D:getlastmodified>Tue, 09 Sep 2025 12:00:00 GMT</D:getlastmodified>\
</D:prop></D:set></D:propertyupdate>";
        let parsed = read_proppatch(body);
        assert_eq!(parsed.names, vec!["getlastmodified".to_owned()]);
        assert_eq!(
            parsed.mtime_text.as_deref(),
            Some("Tue, 09 Sep 2025 12:00:00 GMT")
        );
    }

    #[test]
    fn read_proppatch_reads_win32lastmodifiedtime_under_its_own_namespace() {
        let body = b"<D:propertyupdate xmlns:D=\"DAV:\" xmlns:Z=\"urn:schemas-microsoft-com:\">\
<D:set><D:prop><Z:Win32LastModifiedTime>Tue, 09 Sep 2025 12:00:00 GMT</Z:Win32LastModifiedTime>\
</D:prop></D:set></D:propertyupdate>";
        let parsed = read_proppatch(body);
        assert_eq!(parsed.names, vec!["Win32LastModifiedTime".to_owned()]);
        assert_eq!(
            parsed.mtime_text.as_deref(),
            Some("Tue, 09 Sep 2025 12:00:00 GMT")
        );
    }

    #[test]
    fn read_proppatch_names_an_unrecognised_property_with_no_mtime() {
        let body = b"<D:propertyupdate xmlns:D=\"DAV:\"><D:set><D:prop>\
<D:displayname>New Name</D:displayname></D:prop></D:set></D:propertyupdate>";
        let parsed = read_proppatch(body);
        assert_eq!(parsed.names, vec!["displayname".to_owned()]);
        assert!(parsed.mtime_text.is_none());
    }

    #[test]
    fn proppatch_multistatus_separates_accepted_and_refused_properties() {
        let xml = proppatch_multistatus(
            "Root/a.txt",
            false,
            &["getlastmodified".to_owned()],
            &["displayname".to_owned()],
        );
        assert!(xml.contains("<D:href>/Root/a.txt</D:href>"), "{xml}");
        assert!(xml.contains("<D:getlastmodified/>"), "{xml}");
        assert!(xml.contains("200 OK"), "{xml}");
        assert!(xml.contains("<D:displayname/>"), "{xml}");
        assert!(xml.contains("403 Forbidden"), "{xml}");
    }
}
