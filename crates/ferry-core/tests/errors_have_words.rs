//! Checks that `design/errors.json` stays complete and stays in voice.
//!
//! This test reads `design/errors.json`, every source file in
//! `crates/ferry-core/src`, and every source file in
//! `crates/ferry-runtime/src`, and checks:
//!
//! 1. Every public error enum variant in `ferry-core` has a row.
//! 2. Every `"Runtime::..."` code used in `ferry-runtime` has a row.
//! 3. Every row's three parts are non-empty once trimmed.
//! 4. Every row named after a `ferry-core` enum names a variant that
//!    still exists (this catches a row left behind after a rename).
//! 5. No part uses an exclamation mark, or the words "sorry", "oops", or
//!    "please", per `docs/voice.md`.
//!
//! `ferry-core` depends on no JSON crate and no regex crate, so both the
//! JSON reader and the Rust source reader below are small hand-written
//! parsers, not general ones. They understand the exact shape of the files
//! this repository has today.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The three parts of one error's words, in the order `docs/voice.md` sets:
/// what stopped, why, what to do.
struct Words {
    stopped: String,
    why: String,
    todo: String,
}

fn errors_json_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `crates/ferry-core`. Two levels up is the
    // repository root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("design")
        .join("errors.json")
}

fn core_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn runtime_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ferry-runtime")
        .join("src")
}

// ---------------------------------------------------------------------------
// A small hand-written JSON reader.
//
// `design/errors.json` holds one shape: an object whose values are either
// strings or objects, with only strings inside those objects. That is all
// this reader supports. It is not a general JSON parser.
// ---------------------------------------------------------------------------

enum Json {
    Str(String),
    Obj(Vec<(String, Json)>),
}

struct JsonReader {
    chars: Vec<char>,
    pos: usize,
}

impl JsonReader {
    fn new(source: &str) -> Self {
        JsonReader {
            chars: source.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, expected: char) {
        let got = self.bump();
        assert!(
            got == Some(expected),
            "errors.json: expected '{expected}' at character {}, found {got:?}",
            self.pos
        );
    }

    fn parse_string(&mut self) -> String {
        self.expect('"');
        let mut out = String::new();
        loop {
            let ch = self.bump().expect("errors.json: a string ran off the end");
            match ch {
                '"' => break,
                '\\' => {
                    let escaped = self
                        .bump()
                        .expect("errors.json: an escape ran off the end");
                    match escaped {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'u' => {
                            let mut code = String::new();
                            for _ in 0..4 {
                                code.push(
                                    self.bump()
                                        .expect("errors.json: a short unicode escape"),
                                );
                            }
                            let value = u32::from_str_radix(&code, 16)
                                .expect("errors.json: a bad unicode escape");
                            if let Some(c) = char::from_u32(value) {
                                out.push(c);
                            }
                        }
                        other => panic!("errors.json: unknown escape '\\{other}'"),
                    }
                }
                other => out.push(other),
            }
        }
        out
    }

    fn parse_object(&mut self) -> Vec<(String, Json)> {
        self.expect('{');
        self.skip_whitespace();
        let mut entries = Vec::new();
        if self.peek() == Some('}') {
            self.bump();
            return entries;
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string();
            self.skip_whitespace();
            self.expect(':');
            self.skip_whitespace();
            let value = self.parse_value();
            entries.push((key, value));
            self.skip_whitespace();
            match self.bump() {
                Some(',') => {}
                Some('}') => break,
                other => panic!("errors.json: expected ',' or '}}', found {other:?}"),
            }
        }
        entries
    }

    fn parse_value(&mut self) -> Json {
        self.skip_whitespace();
        match self.peek() {
            Some('"') => Json::Str(self.parse_string()),
            Some('{') => Json::Obj(self.parse_object()),
            other => panic!("errors.json: unexpected character {other:?} at a value"),
        }
    }
}

/// Reads `design/errors.json` into a map from error code to its three
/// words.
fn read_errors_json() -> BTreeMap<String, Words> {
    let path = errors_json_path();
    let content =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let mut reader = JsonReader::new(&content);
    let root = reader.parse_object();

    let errors_value = &root
        .iter()
        .find(|(key, _)| key == "errors")
        .expect("errors.json: no top-level \"errors\" key")
        .1;
    let Json::Obj(rows) = errors_value else {
        panic!("errors.json: \"errors\" is not an object");
    };

    let mut map = BTreeMap::new();
    for (code, value) in rows {
        let Json::Obj(fields) = value else {
            panic!("errors.json: row \"{code}\" is not an object");
        };
        let field = |name: &str| -> String {
            match fields.iter().find(|(key, _)| key == name) {
                Some((_, Json::Str(s))) => s.clone(),
                Some((_, Json::Obj(_))) => panic!("errors.json: \"{code}\".{name} is not a string"),
                None => panic!("errors.json: \"{code}\" has no \"{name}\""),
            }
        };
        map.insert(
            code.clone(),
            Words {
                stopped: field("stopped"),
                why: field("why"),
                todo: field("todo"),
            },
        );
    }
    map
}

// ---------------------------------------------------------------------------
// A small hand-written reader for `ferry-core`'s error enums.
// ---------------------------------------------------------------------------

/// Finds every `pub enum <Name>Error { ... }` block in one source file and
/// returns its variant names as `"NameError::Variant"`.
///
/// This is a hand parser, not a Rust parser. It only understands the shape
/// this crate's error enums are written in: a top-level `pub enum` item,
/// closed by a `}` with no leading spaces, whose direct variant lines are
/// indented by exactly four spaces and start with a capital letter. A
/// struct variant's field lines are indented eight spaces, so they do not
/// match, and neither do attribute or doc comment lines, which start with
/// `#` or `/`.
fn find_core_variants(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut current_enum: Option<&str> = None;

    for line in source.lines() {
        if let Some(name) = parse_enum_header(line) {
            current_enum = Some(name);
            continue;
        }

        if current_enum.is_some() && line == "}" {
            current_enum = None;
            continue;
        }

        if let Some(enum_name) = current_enum
            && let Some(variant) = parse_variant_line(line)
        {
            found.insert(format!("{enum_name}::{variant}"));
        }
    }

    found
}

/// Matches `pub enum FooError {` and returns `"FooError"`. Only an enum
/// whose name ends in "Error" is an error enum; the other enums in these
/// files (`Request`, `Response`, `FileKind`, and so on) are skipped.
fn parse_enum_header(line: &str) -> Option<&str> {
    let name = line.strip_prefix("pub enum ")?.strip_suffix(" {")?;
    if name.ends_with("Error") && name.chars().all(|c| c.is_ascii_alphanumeric()) {
        Some(name)
    } else {
        None
    }
}

/// Matches a variant line: exactly four leading spaces, then a capital
/// letter. Returns the variant's name.
fn parse_variant_line(line: &str) -> Option<&str> {
    let after_indent = line.strip_prefix("    ")?;
    if after_indent.starts_with(' ') {
        return None; // indented more than four spaces, so not a variant
    }
    if !after_indent.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let end = after_indent
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(after_indent.len());
    Some(&after_indent[..end])
}

fn read_core_variants() -> BTreeSet<String> {
    let dir = core_src_dir();
    let mut found = BTreeSet::new();
    let entries =
        fs::read_dir(&dir).unwrap_or_else(|e| panic!("could not read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("could not read a directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
        found.extend(find_core_variants(&source));
    }
    found
}

// ---------------------------------------------------------------------------
// A small reader for `ferry-runtime`'s `"Runtime::..."` string literals.
// ---------------------------------------------------------------------------

/// Finds every `"Runtime::Word"` string literal in one source file.
fn find_runtime_codes(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let chars: Vec<char> = source.chars().collect();
    let marker: Vec<char> = "\"Runtime::".chars().collect();
    let mut i = 0;
    while i + marker.len() <= chars.len() {
        if chars[i..i + marker.len()] == marker[..] {
            let start = i + 1; // the character after the opening quote
            let mut end = start;
            while end < chars.len() && chars[end] != '"' {
                end += 1;
            }
            if end < chars.len() {
                found.insert(chars[start..end].iter().collect());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    found
}

/// Walks a directory tree, collecting every `.rs` file under it.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("could not read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("could not read a directory entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn read_runtime_codes() -> BTreeSet<String> {
    let dir = runtime_src_dir();
    let mut files = Vec::new();
    collect_rs_files(&dir, &mut files);
    let mut found = BTreeSet::new();
    for path in files {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
        found.extend(find_runtime_codes(&source));
    }
    found
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[test]
fn errors_have_words() {
    let words = read_errors_json();
    let core_variants = read_core_variants();
    let runtime_codes = read_runtime_codes();

    // 1. Every core variant has a row in errors.json.
    let missing_core: Vec<&String> = core_variants
        .iter()
        .filter(|v| !words.contains_key(v.as_str()))
        .collect();
    assert!(
        missing_core.is_empty(),
        "these error variants have no row in design/errors.json: {missing_core:?}"
    );

    // 2. Every Runtime:: code found in runtime source has a row.
    let missing_runtime: Vec<&String> = runtime_codes
        .iter()
        .filter(|c| !words.contains_key(c.as_str()))
        .collect();
    assert!(
        missing_runtime.is_empty(),
        "these Runtime:: codes have no row in design/errors.json: {missing_runtime:?}"
    );

    // 3. Every row's three parts are non-empty after trimming.
    let mut empty_parts: Vec<String> = Vec::new();
    for (code, w) in &words {
        if w.stopped.trim().is_empty() {
            empty_parts.push(format!("{code}.stopped"));
        }
        if w.why.trim().is_empty() {
            empty_parts.push(format!("{code}.why"));
        }
        if w.todo.trim().is_empty() {
            empty_parts.push(format!("{code}.todo"));
        }
    }
    assert!(
        empty_parts.is_empty(),
        "these parts of design/errors.json are empty: {empty_parts:?}"
    );

    // 4. Every row named after a core enum names a variant that exists.
    // This catches a row left behind after a variant was renamed or removed.
    let core_enum_names: BTreeSet<&str> = core_variants
        .iter()
        .filter_map(|v| v.split_once("::").map(|(name, _)| name))
        .collect();
    let stale_rows: Vec<&String> = words
        .keys()
        .filter(|code| match code.split_once("::") {
            Some((prefix, _)) => core_enum_names.contains(prefix) && !core_variants.contains(*code),
            None => false,
        })
        .collect();
    assert!(
        stale_rows.is_empty(),
        "these rows in design/errors.json name a variant that no longer exists: {stale_rows:?}"
    );

    // 5. No part uses an exclamation mark, or "sorry", "oops", or "please",
    // in any case. This is docs/voice.md's rule against apology and
    // excitement, made mechanical.
    let banned_words = ["sorry", "oops", "please"];
    let mut voice_violations: Vec<String> = Vec::new();
    for (code, w) in &words {
        for (part_name, part) in [("stopped", &w.stopped), ("why", &w.why), ("todo", &w.todo)] {
            if part.contains('!') {
                voice_violations.push(format!("{code}.{part_name} has an exclamation mark"));
            }
            let lower = part.to_lowercase();
            for banned in banned_words {
                if lower.contains(banned) {
                    voice_violations.push(format!("{code}.{part_name} has the word \"{banned}\""));
                }
            }
        }
    }
    assert!(
        voice_violations.is_empty(),
        "these parts of design/errors.json break the voice rule in docs/voice.md: {voice_violations:?}"
    );
}
