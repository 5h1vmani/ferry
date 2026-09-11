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
    use super::{Item, multistatus, requested_props};

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
}
