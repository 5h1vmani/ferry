//! Remote paths, and the rules that keep them safe.
//!
//! A peer sends paths over the wire. The receiving side turns each one into a
//! real file on disk. Without checks, a peer could ask for `../../etc/passwd`
//! and escape the shared root.
//!
//! A [`RemotePath`] is a path that has passed those checks. The type exists so
//! that the rest of the core cannot forget to run them.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    /// The path had no components, or was an empty string.
    Empty,
    /// The path started with `/`. Remote paths are always relative to a root.
    Absolute,
    /// The path contained a `..` component, which could escape the root.
    ParentComponent,
    /// The path contained a `.` component, which adds nothing and hides intent.
    CurrentComponent,
    /// The path contained a NUL byte, which no supported filesystem accepts.
    NulByte,
    /// The path contained a backslash, which is a separator on some systems.
    Backslash,
    /// The path was longer than [`RemotePath::MAX_LEN`] bytes.
    TooLong,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Empty => "path is empty",
            Self::Absolute => "path is absolute",
            Self::ParentComponent => "path contains a `..` component",
            Self::CurrentComponent => "path contains a `.` component",
            Self::NulByte => "path contains a NUL byte",
            Self::Backslash => "path contains a backslash",
            Self::TooLong => "path is too long",
        };
        f.write_str(text)
    }
}

impl std::error::Error for PathError {}

/// A relative path whose text is safe to join onto a shared root.
///
/// Build one with [`RemotePath::parse`]. The stored form uses `/` as the
/// separator and holds no empty, `.`, or `..` components.
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
    /// # Errors
    ///
    /// Returns the first rule the input broke. See [`PathError`].
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

        let mut parts = Vec::new();
        for part in input.split('/') {
            match part {
                "" => {}
                "." => return Err(PathError::CurrentComponent),
                ".." => return Err(PathError::ParentComponent),
                other => parts.push(other),
            }
        }

        if parts.is_empty() {
            return Err(PathError::Empty);
        }

        Ok(Self(parts.join("/")))
    }

    /// The path as a string, with `/` separators and no leading slash.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path components, in order.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
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
    fn rejects_empty_paths() {
        // The empty string is the only input that reaches this rule.
        assert_eq!(RemotePath::parse(""), Err(PathError::Empty));
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
}
