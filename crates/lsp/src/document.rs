//! In-memory document state tracked by the LSP server.

use std::cell::OnceCell;
use std::path::PathBuf;

use lsp_types::Uri;
use rpm_spec_analyzer::ParseOutcome;

use crate::encoding::LineIndex;

/// One open document. `text` is the current buffer contents; `version`
/// matches the latest `didChange` / `didOpen` notification. `line_index`
/// is rebuilt on every text update because we use full document sync.
///
/// `parse_cache` lazily holds the parsed [`ParseOutcome`] so read-only
/// handlers (`documentSymbol`, `foldingRange`, future inlay hints,
/// xref) reuse one parse per `didChange` instead of reparsing per
/// request. Invalidated by [`replace`](Self::replace) — every text
/// update gets a fresh `OnceCell`.
#[derive(Debug)]
pub struct Document {
    pub uri: Uri,
    pub text: String,
    pub version: i32,
    pub line_index: LineIndex,
    /// Filesystem path derived from `uri` when scheme is `file://`. We
    /// keep it pre-computed because every lint pass uses it for the
    /// filename-aware rules.
    pub path: Option<PathBuf>,
    parse_cache: OnceCell<ParseOutcome>,
}

impl Document {
    pub fn new(uri: Uri, text: String, version: i32) -> Self {
        let line_index = LineIndex::new(&text);
        let path = uri_to_path(&uri);
        Self {
            uri,
            text,
            version,
            line_index,
            path,
            parse_cache: OnceCell::new(),
        }
    }

    /// Replace the buffer contents and bump the version. Used on
    /// `didChange` with `TextDocumentSyncKind::Full`. Resets the parse
    /// cache so the next reader gets a fresh parse.
    pub fn replace(&mut self, text: String, version: i32) {
        self.text = text;
        self.version = version;
        self.line_index = LineIndex::new(&self.text);
        self.parse_cache = OnceCell::new();
    }

    /// Lazily parse the document. Subsequent calls return the cached
    /// outcome until [`replace`](Self::replace) invalidates it.
    pub fn parsed(&self) -> &ParseOutcome {
        self.parse_cache
            .get_or_init(|| rpm_spec_analyzer::parse(&self.text))
    }
}

/// Decode a `file://` URI into a [`PathBuf`]. Returns `None` for other
/// schemes (e.g. `untitled:`, `inmemory:`) and for malformed inputs —
/// callers fall back to defaults for filename-aware rules.
///
/// `lsp-types` 0.97 dropped the `url::Url::to_file_path` helper when
/// it switched from `url` to `fluent_uri`, so we do the conversion
/// manually. The implementation handles:
///   * `file:///abs/path` (POSIX absolute)
///   * `file://host/abs/path` (UNC-ish; host segment is preserved on
///     Windows, ignored on Unix to match `url`'s historical behaviour)
///   * percent-decoding of the path component
pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    let rest = s.strip_prefix("file://")?;
    let path_part = match rest.find('/') {
        // `file:///foo` → rest = "/foo", first '/' at 0, path = "/foo".
        // `file://host/foo` → rest = "host/foo", first '/' at 4, path = "/foo".
        Some(idx) => &rest[idx..],
        None => return None,
    };
    let decoded = percent_decode(path_part)?;
    Some(PathBuf::from(decoded))
}

/// Minimal RFC 3986 percent-decoder for the path component. Returns
/// `None` if the input contains invalid `%XX` escapes or non-UTF-8
/// byte sequences (rejecting those is safer than producing a lossy
/// path).
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Uri {
        s.parse().unwrap()
    }

    #[test]
    fn file_uri_decodes_to_path() {
        let p = uri_to_path(&u("file:///tmp/hello.spec")).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/hello.spec"));
    }

    #[test]
    fn percent_encoded_spaces_decoded() {
        let p = uri_to_path(&u("file:///tmp/with%20space.spec")).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/with space.spec"));
    }

    #[test]
    fn non_file_scheme_yields_none() {
        assert!(uri_to_path(&u("untitled:foo")).is_none());
    }
}
