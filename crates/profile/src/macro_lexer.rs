//! Token-level scanner for RPM macro references.
//!
//! Scans a byte slice for `%`-prefixed macro syntax. Recognises:
//!
//! * `%name` — short form macro reference (identifier scan).
//! * `%{name}` — brace-delimited macro reference.
//! * `%{?name}` — optional-conditional reference (`IfDefined`).
//! * `%{!?name}` — negated optional-conditional (`IfUndefined`).
//! * `%{name:default}` — macro with default value (`has_default = true`).
//! * `%(shell command)` — shell expansion; matched parens may nest.
//! * `%[expr]` — arithmetic expression.
//! * `%%` — literal-percent escape (yields a `LiteralPercent` token).
//!
//! The lexer is intentionally a token scanner only — it does NOT
//! expand macros, evaluate conditionals, run shell, or resolve
//! defaults. Consumers (profile expansion, LSP rename, …) plug their
//! own semantics on top.
//!
//! ## Why a byte slice
//!
//! RPM macro names are ASCII-only by convention. Operating on
//! `&[u8]` keeps cursor arithmetic simple and matches the spec
//! parser's contract. Non-ASCII bytes inside a body are passed
//! through verbatim; the lexer never indexes into the middle of a
//! UTF-8 sequence because it only acts on `%` and ASCII identifier
//! bytes.

use std::ops::Range;

/// One scanned `%`-prefixed token.
///
/// The two ranges are deliberately distinct:
///
/// * [`Self::name_range`] points at the bare identifier (e.g. `foo`
///   in `%{?foo}`) — what an LSP rename wants to highlight.
/// * [`Self::full_range`] covers the entire token, `%` to closing
///   brace/paren/bracket inclusive — what a macro expander wants to
///   advance the input cursor past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroRef<'a> {
    /// Token shape.
    pub kind: MacroKind,
    /// The macro name as a string slice. Empty (`""`) for
    /// [`MacroKind::LiteralPercent`], [`MacroKind::ShellExpansion`],
    /// and [`MacroKind::ArithmeticExpr`] — those tokens have no
    /// `name` in the identifier sense.
    pub name: &'a str,
    /// Byte range covering [`Self::name`] within the input. For
    /// tokens without a name (literal percent / shell / arith) this
    /// is an empty range positioned just past the prefix.
    pub name_range: Range<usize>,
    /// Byte range covering the ENTIRE token in the input, from the
    /// leading `%` through any closing delimiter inclusive.
    pub full_range: Range<usize>,
}

/// Conditional-reference flavour for [`MacroKind::Braced`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conditional {
    /// `%{?name}` — expand only when `name` is defined.
    IfDefined,
    /// `%{!?name}` — expand only when `name` is NOT defined.
    IfUndefined,
}

/// Surface shape of a scanned macro token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroKind {
    /// `%name` — bare-identifier short form.
    Plain,
    /// `%{name}` / `%{?name}` / `%{!?name}` / `%{name:default}`.
    ///
    /// `conditional` is `Some` when the inner body started with `?`
    /// or `!?`; `has_default` is `true` when the inner body had a
    /// `:` after the identifier (default-value form).
    Braced {
        conditional: Option<Conditional>,
        has_default: bool,
    },
    /// `%(shell command)` — matched parens, may nest.
    ShellExpansion,
    /// `%[expression]` — arithmetic context (rpm ≥ 4.14).
    ArithmeticExpr,
    /// `%%` — literal-percent escape. `name` is empty; `full_range`
    /// covers both `%` bytes.
    LiteralPercent,
}

/// Scan one macro reference starting at byte offset `start` (which
/// must point at a `%`). Returns `None` when the input at `start`
/// is not a well-formed token (e.g. a trailing lone `%` at end of
/// input, an unterminated `%{`, an empty identifier).
///
/// Specifically returns `None` on:
///
/// * `start` out of bounds.
/// * `input[start] != b'%'`.
/// * Trailing `%` at end of input.
/// * Unterminated `%{`, `%(`, `%[`.
/// * Empty identifier inside `%{}` (no name to extract).
/// * `%` followed by a byte that is neither an identifier start nor
///   one of `%`, `{`, `(`, `[`.
#[must_use]
pub fn scan_macro_ref(input: &[u8], start: usize) -> Option<MacroRef<'_>> {
    if start >= input.len() || input[start] != b'%' {
        return None;
    }
    // `%%` literal escape.
    if input.get(start + 1) == Some(&b'%') {
        let empty_at = start + 2;
        return Some(MacroRef {
            kind: MacroKind::LiteralPercent,
            name: "",
            name_range: empty_at..empty_at,
            full_range: start..start + 2,
        });
    }
    // `%(shell)` — match nested parens.
    if input.get(start + 1) == Some(&b'(') {
        let end = find_matching(input, start + 1, b'(', b')')?;
        let empty_at = start + 2;
        return Some(MacroRef {
            kind: MacroKind::ShellExpansion,
            name: "",
            name_range: empty_at..empty_at,
            full_range: start..end + 1,
        });
    }
    // `%[expr]` — match nested brackets (RPM does allow nesting in
    // practice via embedded macros).
    if input.get(start + 1) == Some(&b'[') {
        let end = find_matching(input, start + 1, b'[', b']')?;
        let empty_at = start + 2;
        return Some(MacroRef {
            kind: MacroKind::ArithmeticExpr,
            name: "",
            name_range: empty_at..empty_at,
            full_range: start..end + 1,
        });
    }
    // `%{...}` — braced form (plain / conditional / default).
    if input.get(start + 1) == Some(&b'{') {
        return scan_braced(input, start);
    }
    // `%name` — bare identifier.
    let next = *input.get(start + 1)?;
    if !is_ident_start(next) {
        return None;
    }
    let name_start = start + 1;
    let name_end = scan_ident_end(input, name_start);
    let name = std::str::from_utf8(&input[name_start..name_end]).ok()?;
    Some(MacroRef {
        kind: MacroKind::Plain,
        name,
        name_range: name_start..name_end,
        full_range: start..name_end,
    })
}

/// Iterate every `%`-prefixed token in `input` in order. Skips
/// non-`%` bytes silently. Yields literal-percent escapes too —
/// callers that want only "real" references should filter on
/// [`MacroKind`].
///
/// Malformed `%` sequences (lone trailing `%`, `%` followed by an
/// invalid lead byte, unterminated `%{`/`%(`/`%[`) advance the
/// cursor by one byte without yielding a token.
pub fn iter_macro_refs(input: &[u8]) -> MacroRefIter<'_> {
    MacroRefIter { input, cursor: 0 }
}

/// Iterator returned by [`iter_macro_refs`].
#[derive(Debug)]
pub struct MacroRefIter<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Iterator for MacroRefIter<'a> {
    type Item = MacroRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.cursor < self.input.len() {
            if self.input[self.cursor] != b'%' {
                self.cursor += 1;
                continue;
            }
            match scan_macro_ref(self.input, self.cursor) {
                Some(r) => {
                    // Advance past the token. `full_range.end` may
                    // equal `cursor` only for degenerate (empty)
                    // tokens, which the scanner never produces — but
                    // guard against an infinite loop anyway.
                    let end = r.full_range.end;
                    self.cursor = if end > self.cursor {
                        end
                    } else {
                        self.cursor + 1
                    };
                    return Some(r);
                }
                None => {
                    // Malformed `%`-sequence — skip the `%` and
                    // continue. We deliberately don't yield an
                    // "error" token; the caller asked for valid
                    // refs only.
                    self.cursor += 1;
                }
            }
        }
        None
    }
}

/// Scan a `%{...}` braced reference starting at the `%` byte. The
/// caller has already verified `input[start..start+2] == b"%{"`.
fn scan_braced(input: &[u8], start: usize) -> Option<MacroRef<'_>> {
    let mut p = start + 2; // skip `%{`
    let conditional = if input.get(p) == Some(&b'!') && input.get(p + 1) == Some(&b'?') {
        p += 2;
        Some(Conditional::IfUndefined)
    } else if input.get(p) == Some(&b'?') {
        p += 1;
        Some(Conditional::IfDefined)
    } else {
        None
    };
    let name_start = p;
    if name_start >= input.len() || !is_ident_start(input[name_start]) {
        // Empty identifier — `%{}`, `%{?}`, etc. — is not a valid
        // reference.
        return None;
    }
    let name_end = scan_ident_end(input, name_start);
    // After the identifier we expect either `}` (plain / conditional)
    // or `:` (default-value form), followed eventually by `}`.
    let mut q = name_end;
    let has_default = match input.get(q) {
        Some(&b'}') => false,
        Some(&b':') => {
            // Walk until the matching `}` — defaults may contain
            // arbitrary bytes (including nested `%{…}` expansions).
            // We don't recurse into them; just find the closer.
            q += 1;
            let mut depth = 1; // we're inside the `%{`
            while q < input.len() && depth > 0 {
                match input[q] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                q += 1;
            }
            if depth != 0 {
                return None;
            }
            // q now points one past the closing `}`. Step back so
            // the rest of the function can use the same convention
            // as the non-default branch (q points AT the `}`).
            q -= 1;
            true
        }
        _ => {
            // Either end-of-input or some other byte (e.g. `\n`).
            // The original `rename::scan::parse_braced` walked
            // forward looking for `}` even past non-ident bytes —
            // do the same for behavioural parity. Anything between
            // the identifier and the `}` is treated as an opaque
            // tail (callers that care can re-scan).
            while q < input.len() && input[q] != b'}' {
                q += 1;
            }
            if q >= input.len() {
                return None;
            }
            false
        }
    };
    let name = std::str::from_utf8(&input[name_start..name_end]).ok()?;
    Some(MacroRef {
        kind: MacroKind::Braced {
            conditional,
            has_default,
        },
        name,
        name_range: name_start..name_end,
        full_range: start..q + 1,
    })
}

/// Return the exclusive end of the identifier starting at `start`.
/// Assumes `input[start]` is a valid ident-start byte; callers must
/// check first.
fn scan_ident_end(input: &[u8], start: usize) -> usize {
    let mut e = start + 1;
    while e < input.len() && is_ident_byte(input[e]) {
        e += 1;
    }
    e
}

/// `true` for the first byte of an RPM macro identifier.
#[must_use]
pub fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// `true` for subsequent bytes of an RPM macro identifier.
#[must_use]
pub fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scan an identifier starting at `start`. Returns the exclusive end
/// position, or `None` if `bytes[start]` is not an identifier start.
///
/// Exposed for callers that need to scan additional identifiers
/// AFTER a [`MacroRef`] (e.g. `%define NAME` — the lexer reports
/// `%define` as a [`MacroKind::Plain`] token, and the caller scans
/// the operand from `full_range.end` onward).
#[must_use]
pub fn scan_ident(input: &[u8], start: usize) -> Option<usize> {
    if start >= input.len() || !is_ident_start(input[start]) {
        return None;
    }
    Some(scan_ident_end(input, start))
}

/// Find the index of the matching closing delimiter starting at
/// `open` (which points AT the opening delimiter). Handles nesting.
/// Returns the index of the closing byte, or `None` if unmatched.
fn find_matching(input: &[u8], open: usize, open_b: u8, close_b: u8) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < input.len() {
        if input[i] == open_b {
            depth += 1;
        } else if input[i] == close_b {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(src: &str) -> Vec<MacroRef<'_>> {
        iter_macro_refs(src.as_bytes()).collect()
    }

    #[test]
    fn plain_short_form() {
        let got = refs("hello %foo bar");
        assert_eq!(got.len(), 1);
        let r = &got[0];
        assert_eq!(r.kind, MacroKind::Plain);
        assert_eq!(r.name, "foo");
        assert_eq!(r.name_range, 7..10);
        assert_eq!(r.full_range, 6..10);
    }

    #[test]
    fn braced_plain() {
        let got = refs("a %{foo} b");
        assert_eq!(got.len(), 1);
        let r = &got[0];
        assert_eq!(
            r.kind,
            MacroKind::Braced {
                conditional: None,
                has_default: false
            }
        );
        assert_eq!(r.name, "foo");
        assert_eq!(&"a %{foo} b"[r.name_range.clone()], "foo");
        assert_eq!(&"a %{foo} b"[r.full_range.clone()], "%{foo}");
    }

    #[test]
    fn braced_if_defined() {
        let got = refs("%{?foo}");
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].kind,
            MacroKind::Braced {
                conditional: Some(Conditional::IfDefined),
                has_default: false
            }
        );
        assert_eq!(got[0].name, "foo");
    }

    #[test]
    fn braced_if_undefined() {
        let got = refs("%{!?foo}");
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].kind,
            MacroKind::Braced {
                conditional: Some(Conditional::IfUndefined),
                has_default: false
            }
        );
        assert_eq!(got[0].name, "foo");
    }

    #[test]
    fn braced_with_default() {
        let got = refs("%{foo:default}");
        assert_eq!(got.len(), 1);
        let r = &got[0];
        assert_eq!(
            r.kind,
            MacroKind::Braced {
                conditional: None,
                has_default: true
            }
        );
        assert_eq!(r.name, "foo");
        assert_eq!(&"%{foo:default}"[r.full_range.clone()], "%{foo:default}");
    }

    #[test]
    fn braced_conditional_with_default() {
        let got = refs("%{?foo:fallback}");
        assert_eq!(got.len(), 1);
        let r = &got[0];
        assert_eq!(
            r.kind,
            MacroKind::Braced {
                conditional: Some(Conditional::IfDefined),
                has_default: true
            }
        );
        assert_eq!(r.name, "foo");
    }

    #[test]
    fn shell_expansion_nested_parens() {
        // The outer `%(...)` must be reported as ONE ShellExpansion
        // token; nested `%{nested}` is part of the body and not
        // separately scanned (iter would skip past the entire token).
        let src = "before %(echo (a) %{nested}) after";
        let got = refs(src);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, MacroKind::ShellExpansion);
        assert_eq!(&src[got[0].full_range.clone()], "%(echo (a) %{nested})");
    }

    #[test]
    fn arithmetic_expr() {
        let src = "%[1 + 2]";
        let got = refs(src);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, MacroKind::ArithmeticExpr);
        assert_eq!(&src[got[0].full_range.clone()], "%[1 + 2]");
    }

    #[test]
    fn literal_percent() {
        let src = "100%% done %foo";
        let got = refs(src);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].kind, MacroKind::LiteralPercent);
        assert_eq!(got[0].name, "");
        assert_eq!(got[0].full_range, 3..5);
        assert_eq!(got[1].kind, MacroKind::Plain);
        assert_eq!(got[1].name, "foo");
    }

    #[test]
    fn multiple_refs_in_one_input() {
        let src = "%a %{b} %{?c} %{!?d} %{e:f}";
        let got = refs(src);
        let kinds_names: Vec<(&str, MacroKind)> = got.iter().map(|r| (r.name, r.kind)).collect();
        assert_eq!(
            kinds_names,
            vec![
                ("a", MacroKind::Plain),
                (
                    "b",
                    MacroKind::Braced {
                        conditional: None,
                        has_default: false
                    }
                ),
                (
                    "c",
                    MacroKind::Braced {
                        conditional: Some(Conditional::IfDefined),
                        has_default: false
                    }
                ),
                (
                    "d",
                    MacroKind::Braced {
                        conditional: Some(Conditional::IfUndefined),
                        has_default: false
                    }
                ),
                (
                    "e",
                    MacroKind::Braced {
                        conditional: None,
                        has_default: true
                    }
                ),
            ]
        );
    }

    #[test]
    fn malformed_unterminated_brace() {
        // `%{foo` with no `}` — must NOT yield a token.
        let got = refs("hello %{foo");
        assert!(got.is_empty());
    }

    #[test]
    fn malformed_unterminated_paren() {
        let got = refs("hello %(echo foo");
        assert!(got.is_empty());
    }

    #[test]
    fn malformed_unterminated_bracket() {
        let got = refs("hello %[1 + 2");
        assert!(got.is_empty());
    }

    #[test]
    fn empty_identifier_in_braces_returns_none() {
        // `%{}` and `%{?}` have no identifier — not a valid ref.
        assert!(refs("%{}").is_empty());
        assert!(refs("%{?}").is_empty());
        assert!(refs("%{!?}").is_empty());
    }

    #[test]
    fn trailing_percent_at_end_of_input() {
        let got = refs("foo %");
        assert!(got.is_empty());
    }

    #[test]
    fn lone_percent_followed_by_non_ident() {
        // `%-foo` — `-` is neither `%`/`{`/`(`/`[` nor an ident
        // start. Scanner must skip the `%` without yielding.
        let got = refs("a %-foo b");
        assert!(got.is_empty());
    }

    #[test]
    fn scan_macro_ref_at_offset() {
        let src = b"xx %foo yy";
        let r = scan_macro_ref(src, 3).expect("ref at 3");
        assert_eq!(r.name, "foo");
        assert_eq!(r.full_range, 3..7);
        // At an offset that is not a `%` byte — None.
        assert!(scan_macro_ref(src, 0).is_none());
    }

    #[test]
    fn scan_macro_ref_out_of_bounds() {
        let src = b"abc";
        assert!(scan_macro_ref(src, 99).is_none());
    }

    #[test]
    fn iter_skips_non_percent_text_quickly() {
        // Sanity: a body with no `%` yields nothing and terminates.
        let big = "a".repeat(10_000);
        assert!(refs(&big).is_empty());
    }

    #[test]
    fn scan_ident_helper_after_define() {
        // Caller use: `%define foo` — lexer reports `%define`, then
        // the caller scans forward for whitespace + identifier.
        let src = b"%define   bar";
        let mac = scan_macro_ref(src, 0).expect("%define");
        assert_eq!(mac.name, "define");
        // Skip whitespace.
        let mut p = mac.full_range.end;
        while p < src.len() && (src[p] == b' ' || src[p] == b'\t') {
            p += 1;
        }
        let end = scan_ident(src, p).expect("operand");
        assert_eq!(std::str::from_utf8(&src[p..end]).unwrap(), "bar");
    }

    #[test]
    fn name_range_excludes_braces_and_prefix() {
        // The whole point of having two ranges: an LSP rename wants
        // to highlight ONLY the identifier, not `%{?...}`.
        let src = "%{!?foo_bar}";
        let r = &refs(src)[0];
        assert_eq!(&src[r.name_range.clone()], "foo_bar");
        assert_eq!(&src[r.full_range.clone()], src);
    }

    #[test]
    fn default_body_with_nested_brace() {
        // `%{foo:%{bar}}` — the default value contains a nested
        // brace pair. The lexer must find the OUTER `}`.
        let src = "%{foo:%{bar}}";
        let got = refs(src);
        assert_eq!(got.len(), 1);
        let r = &got[0];
        assert_eq!(r.name, "foo");
        assert!(matches!(
            r.kind,
            MacroKind::Braced {
                has_default: true,
                ..
            }
        ));
        assert_eq!(&src[r.full_range.clone()], "%{foo:%{bar}}");
    }

    #[test]
    fn identifier_chars_stop_at_non_ident() {
        let src = "%foo-bar";
        let got = refs(src);
        // `%foo` is the ref; `-bar` is trailing text.
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "foo");
        assert_eq!(got[0].full_range, 0..4);
    }
}
