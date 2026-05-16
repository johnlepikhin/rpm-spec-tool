//! Line-level shell tokenizer.
//!
//! The tokenizer is intentionally **naïve**: it splits a `Text` line
//! into shell-word tokens, honouring single (`'`) and double (`"`)
//! quoting, but does not implement command substitution, variable
//! expansion, here-documents, redirection grammar, or any of the
//! full shell language. A full shell AST is out of scope (see Phase 25
//! in the roadmap).
//!
//! Macro references inside a `Text` line are preserved verbatim: a
//! token may consist of literal bytes plus one or more macro segments
//! (e.g. `%{buildroot}/usr/bin/foo` is one token with three segments).
//! Callers that only want the literal join (without macros) use
//! [`ShellToken::literal_str`]; callers that need the macro-resolved
//! literal use [`ShellToken::flatten_with`] with a resolver closure.
//!
//! Quoting rules:
//! - Single-quoted strings are taken verbatim (no macro interpretation
//!   *as far as the tokenizer is concerned* — RPM expands macros
//!   inside strings at parse time, before the shell sees them, so the
//!   tokenizer accepts the segments as the parser produced them).
//! - Double-quoted strings remain one token; whitespace inside does
//!   not split.
//! - Backslash before whitespace escapes the split.
//! - Comment marker `#` at a word start terminates the line; bytes
//!   after it are dropped.

use rpm_spec::ast::{Text, TextSegment};

/// One shell-word token from a line.
///
/// The token's content is exposed as a sequence of [`ShellArg`] pieces
/// so macro references survive verbatim. Most rules only need the
/// literal flattening — see [`ShellToken::literal_str`].
#[derive(Debug, Clone)]
pub struct ShellToken {
    /// Pieces that make up the token, in source order.
    pub parts: Vec<ShellArg>,
}

/// One piece of a shell token. Either a literal byte slice (the
/// tokenizer's own splitting respects quoting) or a macro reference
/// carried through from the AST.
#[derive(Debug, Clone)]
pub enum ShellArg {
    /// Plain literal text — no macros.
    Literal(String),
    /// `%foo` / `%{foo}` / `%(...)` etc. — the parser's verbatim
    /// macro name, suitable for `Profile::macros.expand_to_literal`.
    Macro(String),
}

impl ShellToken {
    /// If every part is `ShellArg::Literal`, return the concatenated
    /// text. `None` when any macro is present — call sites that need
    /// to inspect the literal must decide on a fallback (skip
    /// classification, attempt resolution against a profile, etc.).
    pub fn literal_str(&self) -> Option<String> {
        let mut out = String::new();
        for p in &self.parts {
            match p {
                ShellArg::Literal(s) => out.push_str(s),
                ShellArg::Macro(_) => return None,
            }
        }
        Some(out)
    }

    /// Best-effort literal: literal parts are joined, macro parts are
    /// rendered as `%{name}` so the resulting string still reads like
    /// the source. Used for diagnostic messages and prefix checks
    /// that should *not* silently drop macros.
    pub fn render_verbatim(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            match p {
                ShellArg::Literal(s) => out.push_str(s),
                ShellArg::Macro(name) => {
                    out.push_str("%{");
                    out.push_str(name);
                    out.push('}');
                }
            }
        }
        out
    }
}

/// Split one `ShellBody` line into shell-word tokens.
///
/// Returns an empty vector for blank or comment-only lines. The split
/// is intentionally tolerant: malformed quoting (an unclosed `'`) is
/// treated as "everything until end of line is one token" rather than
/// aborting — the linter must keep running on imperfect input.
pub fn tokenize_line(line: &Text) -> Vec<ShellToken> {
    let mut tokens = Vec::new();
    let mut current = ShellToken { parts: Vec::new() };
    let mut current_literal = String::new();
    let mut state = State::Outside;

    for seg in &line.segments {
        match seg {
            TextSegment::Literal(s) => {
                tokenize_literal_chunk(
                    s,
                    &mut state,
                    &mut current,
                    &mut current_literal,
                    &mut tokens,
                );
            }
            TextSegment::Macro(m) => {
                // Macros never split a word, irrespective of quoting:
                // RPM expanded them before the shell sees the line.
                // Flush any accumulated literal first to keep parts
                // in source order.
                flush_literal(&mut current_literal, &mut current);
                current.parts.push(ShellArg::Macro(m.name.clone()));
            }
            _ => {}
        }
    }
    flush_literal(&mut current_literal, &mut current);
    if !current.parts.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Between tokens — whitespace splits.
    Outside,
    /// Inside an unquoted token.
    Unquoted,
    /// Inside `'…'` — every byte is literal until the closing quote.
    Single,
    /// Inside `"…"` — whitespace stays, but backslash escapes.
    Double,
}

fn tokenize_literal_chunk(
    s: &str,
    state: &mut State,
    current: &mut ShellToken,
    literal: &mut String,
    tokens: &mut Vec<ShellToken>,
) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match *state {
            State::Outside => {
                if b.is_ascii_whitespace() {
                    i += 1;
                    continue;
                }
                if b == b'#' {
                    // Start-of-word `#` terminates the line.
                    return;
                }
                *state = match b {
                    b'\'' => State::Single,
                    b'"' => State::Double,
                    _ => {
                        literal.push(b as char);
                        State::Unquoted
                    }
                };
                // All three transitions advance one byte; the
                // unquoted case has already pushed the literal byte.
                i += 1;
            }
            State::Unquoted => {
                if b.is_ascii_whitespace() {
                    finish_token(literal, current, tokens);
                    *state = State::Outside;
                    i += 1;
                    continue;
                }
                match b {
                    b'\'' => {
                        *state = State::Single;
                        i += 1;
                    }
                    b'"' => {
                        *state = State::Double;
                        i += 1;
                    }
                    b'\\' if i + 1 < bytes.len() => {
                        // Backslash-escape the next byte.
                        literal.push(bytes[i + 1] as char);
                        i += 2;
                    }
                    _ => {
                        literal.push(b as char);
                        i += 1;
                    }
                }
            }
            State::Single => {
                if b == b'\'' {
                    *state = State::Unquoted;
                    i += 1;
                } else {
                    literal.push(b as char);
                    i += 1;
                }
            }
            State::Double => {
                match b {
                    b'"' => {
                        *state = State::Unquoted;
                        i += 1;
                    }
                    b'\\' if i + 1 < bytes.len() => {
                        // In double quotes only `\` `\"` `\$` `\`` are
                        // special; we treat any backslash-X as the
                        // byte X. Good enough for command extraction.
                        literal.push(bytes[i + 1] as char);
                        i += 2;
                    }
                    _ => {
                        literal.push(b as char);
                        i += 1;
                    }
                }
            }
        }
    }
}

fn finish_token(literal: &mut String, current: &mut ShellToken, tokens: &mut Vec<ShellToken>) {
    flush_literal(literal, current);
    if !current.parts.is_empty() {
        tokens.push(std::mem::replace(current, ShellToken { parts: Vec::new() }));
    }
}

fn flush_literal(literal: &mut String, current: &mut ShellToken) {
    if !literal.is_empty() {
        current
            .parts
            .push(ShellArg::Literal(std::mem::take(literal)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec::ast::{ConditionalMacro, MacroKind, MacroRef, Text, TextSegment};

    fn t(src: &str) -> Text {
        Text::from(src)
    }

    fn lit(src: &str) -> ShellArg {
        ShellArg::Literal(src.to_owned())
    }

    fn mac(name: &str) -> ShellArg {
        ShellArg::Macro(name.to_owned())
    }

    fn macro_text(literal_prefix: &str, macro_name: &str, literal_suffix: &str) -> Text {
        Text {
            segments: vec![
                TextSegment::Literal(literal_prefix.into()),
                TextSegment::macro_ref(MacroRef {
                    kind: MacroKind::Braced,
                    name: macro_name.into(),
                    args: Vec::new(),
                    conditional: ConditionalMacro::None,
                    with_value: None,
                }),
                TextSegment::Literal(literal_suffix.into()),
            ],
        }
    }

    #[test]
    fn splits_on_whitespace() {
        let toks = tokenize_line(&t("rm -rf /tmp/foo"));
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].literal_str().as_deref(), Some("rm"));
        assert_eq!(toks[1].literal_str().as_deref(), Some("-rf"));
        assert_eq!(toks[2].literal_str().as_deref(), Some("/tmp/foo"));
    }

    #[test]
    fn empty_line_yields_no_tokens() {
        assert!(tokenize_line(&t("")).is_empty());
        assert!(tokenize_line(&t("   ")).is_empty());
    }

    #[test]
    fn comment_terminates_line() {
        let toks = tokenize_line(&t("echo hi # ignored"));
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].literal_str().as_deref(), Some("echo"));
        assert_eq!(toks[1].literal_str().as_deref(), Some("hi"));
    }

    #[test]
    fn single_quotes_preserve_whitespace() {
        let toks = tokenize_line(&t("echo 'hello world'"));
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].literal_str().as_deref(), Some("hello world"));
    }

    #[test]
    fn double_quotes_preserve_whitespace() {
        let toks = tokenize_line(&t("install -m 0644 \"a b.txt\" /etc/"));
        assert_eq!(toks.len(), 5);
        assert_eq!(toks[3].literal_str().as_deref(), Some("a b.txt"));
    }

    #[test]
    fn backslash_escapes_space() {
        let toks = tokenize_line(&t("touch foo\\ bar"));
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].literal_str().as_deref(), Some("foo bar"));
    }

    #[test]
    fn macro_keeps_part_in_token() {
        // `cp %{buildroot}/etc/foo /etc/foo` → 3 tokens, second has
        // macro + literal.
        let line = Text {
            segments: vec![
                TextSegment::Literal("cp ".into()),
                TextSegment::macro_ref(MacroRef {
                    kind: MacroKind::Braced,
                    name: "buildroot".into(),
                    args: Vec::new(),
                    conditional: ConditionalMacro::None,
                    with_value: None,
                }),
                TextSegment::Literal("/etc/foo /etc/foo".into()),
            ],
        };
        let toks = tokenize_line(&line);
        assert_eq!(toks.len(), 3);
        assert!(toks[0].literal_str().as_deref() == Some("cp"));
        // Token 1 has a macro segment: `literal_str` returns None,
        // `render_verbatim` re-emits the `%{…}` form.
        assert!(toks[1].literal_str().is_none());
        assert_eq!(toks[1].render_verbatim(), "%{buildroot}/etc/foo");
        assert_eq!(toks[2].literal_str().as_deref(), Some("/etc/foo"));
    }

    #[test]
    fn unclosed_quote_consumes_rest_of_line() {
        // Tolerant tokenizer: malformed input must not abort linting.
        let toks = tokenize_line(&t("echo 'no closing quote"));
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].literal_str().as_deref(), Some("no closing quote"));
    }

    #[test]
    fn render_verbatim_preserves_macro_in_word() {
        let line = macro_text("foo-", "version", "-bar");
        let toks = tokenize_line(&line);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].render_verbatim(), "foo-%{version}-bar");
        assert!(toks[0].literal_str().is_none());
    }

    #[test]
    fn smoke_unused_helper_imports() {
        // Touches the helpers used only in macro_text/lit/mac so the
        // dead-code lint doesn't fire on tokenize_line-style tests.
        let _ = (lit("x"), mac("y"));
    }
}
