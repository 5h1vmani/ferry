//! Remote paths, and the rules that keep them safe.
//!
//! A peer sends paths over the wire. The receiving side turns each one into a
//! real file on disk. Without checks, a peer could ask for `../../etc/passwd`
//! and escape the shared root.
//!
//! A [`RemotePath`] is a path that has passed those checks. The type exists so
//! that the rest of the core cannot forget to run them.
//!
//! # The shared root
//!
//! The empty string names the shared root itself. [`RemotePath::parse`]
//! accepts it and builds a path with no components at all. Call
//! [`RemotePath::is_root`] to tell that path apart from every other one.
//! Most operations still refuse the root. Each layer that does says so where
//! it checks.
//!
//! # What this module does not do
//!
//! These checks are lexical. They read the text of the path and nothing else.
//! They cannot see what the filesystem resolves.
//!
//! A symlink inside the shared root that points outside it passes every check
//! here. So does a FIFO, which blocks the reading thread forever, and so does a
//! device file. The test
//! `a_validated_path_can_still_escape_through_a_symlink` records this gap.
//!
//! The filesystem layer must therefore do three more things:
//!
//! 1. Open every path with `O_NOFOLLOW_ANY` on macOS, and with `O_NOFOLLOW` on
//!    each component on Android.
//! 2. Refuse anything that is not a regular file or a directory.
//! 3. Resolve the result and confirm it still sits inside the shared root.

use std::fmt;

/// The reason a path was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The path named the shared root, and the operation that checked it
    /// does not accept the root.
    ///
    /// [`RemotePath::parse`] no longer returns this. The empty string parses
    /// as the root, since [`RemotePath::is_root`] is how a caller tells the
    /// root apart from every other path. This variant now fires only where a
    /// caller reads that flag itself and refuses to go on, such as
    /// `Transfer::new` in `crate::session` refusing a root source or
    /// destination.
    #[error("path is empty")]
    Empty,
    /// The path started with `/`. Remote paths are always relative to a root.
    #[error("path is absolute")]
    Absolute,
    /// The path contained a `..` component, which could escape the root.
    #[error("path contains a `..` component")]
    ParentComponent,
    /// The path contained a `.` component, which adds nothing and hides intent.
    #[error("path contains a `.` component")]
    CurrentComponent,
    /// The path contained a NUL byte, which no supported filesystem accepts.
    #[error("path contains a NUL byte")]
    NulByte,
    /// The path contained a backslash, which is a separator on some systems.
    #[error("path contains a backslash")]
    Backslash,
    /// The path was longer than [`RemotePath::MAX_LEN`] bytes.
    #[error("path is too long")]
    TooLong,
}

/// A relative path whose text is safe to join onto a shared root.
///
/// Build one with [`RemotePath::parse`]. The stored form uses `/` as the
/// separator and holds no empty, `.`, or `..` components, except for the one
/// path that has no components at all: the shared root. Test for that case
/// with [`RemotePath::is_root`].
///
/// This is a lexical guarantee only. It does not make the path safe to open.
/// See the module documentation for what the filesystem layer must still do.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemotePath(String);

impl RemotePath {
    /// The longest path Ferry accepts, in bytes.
    ///
    /// Common filesystems stop somewhere between 1024 and 4096 bytes. This
    /// limit sits below all of them, and it caps how much memory one frame can
    /// force the receiver to hold.
    pub const MAX_LEN: usize = 1024;

    /// Check a path from the wire and normalise its separators.
    ///
    /// The empty string is accepted and names the shared root. Every other
    /// input still runs through the same checks as before: no leading `/`, no
    /// `.` or `..` component, no NUL byte, no backslash, and no more than
    /// [`RemotePath::MAX_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// Returns the first rule the input broke. See [`PathError`]. Note that
    /// [`PathError::Empty`] is never returned here; see its own
    /// documentation for where it still applies.
    pub fn parse(input: &str) -> Result<Self, PathError> {
        if input.len() > Self::MAX_LEN {
            return Err(PathError::TooLong);
        }
        if input.contains('\0') {
            return Err(PathError::NulByte);
        }
        if input.contains('\\') {
            return Err(PathError::Backslash);
        }
        if input.starts_with('/') {
            return Err(PathError::Absolute);
        }
        if input.is_empty() {
            return Ok(Self(String::new()));
        }

        let mut parts = Vec::new();
        for part in input.split('/') {
            match part {
                "" => {}
                "." => return Err(PathError::CurrentComponent),
                ".." => return Err(PathError::ParentComponent),
                other => parts.push(other),
            }
        }

        Ok(Self(parts.join("/")))
    }

    /// The path as a string, with `/` separators and no leading slash. The
    /// root's string form is the empty string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when this path names the shared root itself, rather than
    /// anything inside it.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The path components, in order. Empty for the root.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|part| !part.is_empty())
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{PathError, RemotePath};

    #[test]
    fn accepts_a_plain_relative_path() {
        let path = RemotePath::parse("DCIM/Camera/IMG_0001.jpg").unwrap();
        assert_eq!(path.as_str(), "DCIM/Camera/IMG_0001.jpg");
    }

    #[test]
    fn collapses_repeated_separators() {
        let path = RemotePath::parse("DCIM//Camera///a.jpg").unwrap();
        assert_eq!(path.as_str(), "DCIM/Camera/a.jpg");
    }

    #[test]
    fn drops_a_trailing_separator() {
        let path = RemotePath::parse("Download/").unwrap();
        assert_eq!(path.as_str(), "Download");
    }

    #[test]
    fn rejects_traversal() {
        assert_eq!(
            RemotePath::parse("../etc/passwd"),
            Err(PathError::ParentComponent)
        );
        assert_eq!(
            RemotePath::parse("a/../../b"),
            Err(PathError::ParentComponent)
        );
        assert_eq!(RemotePath::parse(".."), Err(PathError::ParentComponent));
    }

    #[test]
    fn rejects_absolute_paths() {
        assert_eq!(RemotePath::parse("/etc/passwd"), Err(PathError::Absolute));
        // A path of only separators is caught by the leading-slash rule first.
        assert_eq!(RemotePath::parse("///"), Err(PathError::Absolute));
    }

    #[test]
    fn the_empty_string_parses_as_the_root() {
        let root = RemotePath::parse("").unwrap();
        assert_eq!(root.as_str(), "");
        assert!(root.is_root());
        assert_eq!(root.components().count(), 0);
    }

    #[test]
    fn a_path_with_real_components_is_not_the_root() {
        let path = RemotePath::parse("DCIM").unwrap();
        assert!(!path.is_root());
    }

    #[test]
    fn rejects_dot_components() {
        assert_eq!(RemotePath::parse("./a"), Err(PathError::CurrentComponent));
    }

    #[test]
    fn rejects_nul_and_backslash() {
        assert_eq!(RemotePath::parse("a\0b"), Err(PathError::NulByte));
        assert_eq!(RemotePath::parse("a\\b"), Err(PathError::Backslash));
    }

    #[test]
    fn rejects_paths_over_the_limit() {
        let long = "a".repeat(RemotePath::MAX_LEN + 1);
        assert_eq!(RemotePath::parse(&long), Err(PathError::TooLong));
    }

    #[test]
    fn a_validated_path_can_still_escape_through_a_symlink() {
        // This test records a known gap rather than a bug. RemotePath checks
        // text only. Closing the gap is the filesystem layer's job, using
        // O_NOFOLLOW_ANY. If this test ever starts failing, the checks grew
        // beyond lexical and the module documentation needs updating.
        let base = std::env::temp_dir().join(format!("ferry-symlink-{}", std::process::id()));
        let root = base.join("root");
        let outside = base.join("outside");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"private").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let path = RemotePath::parse("link/secret.txt").expect("the text is valid");
        let joined = root.join(path.as_str());
        assert!(joined.starts_with(&root), "the lexical check passes");

        let resolved = std::fs::canonicalize(&joined).unwrap();
        let real_root = std::fs::canonicalize(&root).unwrap();
        assert!(
            !resolved.starts_with(&real_root),
            "a lexically valid path escaped the root through a symlink"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn no_accepted_path_can_escape_a_root_lexically() {
        // Any path the parser accepts must stay inside the root once joined.
        let root = std::path::Path::new("/tmp/ferry-root");
        for candidate in [
            "a",
            "a/b",
            "a//b/",
            "DCIM/Camera/x.jpg",
            "..a",
            "a..b",
            "a.",
        ] {
            if let Ok(path) = RemotePath::parse(candidate) {
                let joined = root.join(path.as_str());
                assert!(joined.starts_with(root), "escaped root: {candidate}");
                assert!(
                    !path
                        .components()
                        .any(|c| c == ".." || c == "." || c.is_empty()),
                    "unsafe component survived: {candidate}"
                );
            }
        }
    }

    // Property tests. `RemotePath::parse` is the only gate between a peer's
    // text and the filesystem, so these check it against generated
    // adversarial input, not only the fixed cases above.

    /// Segments built only from characters that cannot trigger a rejection on
    /// their own. Inserting `..` among them isolates the one rule a test
    /// cares about.
    fn safe_segment() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_]{1,8}"
    }

    /// A path built from safe segments, with a `..` component spliced in at
    /// an arbitrary position among them.
    fn path_with_a_parent_component_inserted() -> impl Strategy<Value = String> {
        (proptest::collection::vec(safe_segment(), 0..5), 0..=5_usize).prop_map(
            |(mut segments, at)| {
                let at = at.min(segments.len());
                segments.insert(at, "..".to_owned());
                segments.join("/")
            },
        )
    }

    /// A mix of fully arbitrary text and text biased toward looking like a
    /// path. The properties below run over both, so a case can be pure noise
    /// or something close to a real path.
    fn arbitrary_or_path_like_string() -> impl Strategy<Value = String> {
        prop_oneof![".*", "[a-zA-Z0-9._/\\\\-]{0,64}"]
    }

    proptest! {
        #[test]
        fn never_lets_an_accepted_path_hold_an_unsafe_component(
            input in arbitrary_or_path_like_string()
        ) {
            // This is the core invariant the module exists to keep. No
            // component may equal `..` or `.` or be empty, whatever text a
            // peer sends.
            if let Ok(path) = RemotePath::parse(&input) {
                prop_assert!(
                    path.components().all(|c| c != ".." && c != "." && !c.is_empty())
                );
                prop_assert!(!path.as_str().starts_with('/'));
                prop_assert!(!path.as_str().contains('\0'));
                prop_assert!(!path.as_str().contains('\\'));
                prop_assert!(path.as_str().len() <= RemotePath::MAX_LEN);
            }
        }

        #[test]
        fn never_lets_an_accepted_path_escape_a_joined_root(
            input in arbitrary_or_path_like_string()
        ) {
            // A path that parsing accepts gets joined onto the shared root
            // later. If that join could ever land outside the root, the
            // parser's checks would not be enough to contain a peer.
            if let Ok(path) = RemotePath::parse(&input) {
                let root = std::path::Path::new("/root");
                let joined = root.join(path.as_str());
                prop_assert!(joined.starts_with(root));
            }
        }

        #[test]
        fn reaches_a_fixed_point_so_parsing_twice_is_a_no_op(
            input in arbitrary_or_path_like_string()
        ) {
            // A normaliser that has not reached a fixed point in one pass
            // could still hide a `..` behind a second rewrite. Parsing an
            // already-accepted path again must return the same path.
            if let Ok(path) = RemotePath::parse(&input) {
                let reparsed = RemotePath::parse(path.as_str()).unwrap();
                prop_assert_eq!(reparsed, path);
            }
        }

        #[test]
        fn always_rejects_a_dot_dot_component_at_any_position(
            input in path_with_a_parent_component_inserted()
        ) {
            // The fixed tests above only try `..` at the start or in the
            // middle of one short path. This tries every position in
            // otherwise-safe paths, so no position can slip through.
            prop_assert_eq!(RemotePath::parse(&input), Err(PathError::ParentComponent));
        }
    }
}
