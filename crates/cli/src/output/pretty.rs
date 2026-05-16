//! ANSI-coloured pretty-printing via [`rpm_spec::printer::print_to`].
//!
//! Implements [`PrintWriter`] over any [`WriteColor`] sink so each
//! emitted chunk gets the colour mapped from the active [`Theme`].
//! When the sink is not colour-capable (piped output, `--color never`),
//! `set_color` / `reset` become no-ops and the bytes pass through
//! verbatim — round-trip stays byte-identical.

use std::io;

use codespan_reporting::term::termcolor::{Color, ColorSpec, WriteColor};
use rpm_spec::printer::{PrintWriter, TokenKind};

/// 256-colour palette entry for "bright black" / grey, used for muted
/// operators and comments in the default theme.
const GREY: Color = Color::Ansi256(8);

/// Mapping from [`TokenKind`] to terminal colour. Entries that have
/// no styled rendering are stored as `None` and emitted plain.
///
/// Construct via [`Theme::dark`] or a future palette factory. The
/// type intentionally does **not** implement [`Default`] — an all-
/// `None` theme would silently disable highlighting, which is almost
/// never what a caller wants.
#[derive(Debug, Clone)]
pub struct Theme {
    tag_name: Option<ColorSpec>,
    tag_qualifier: Option<ColorSpec>,
    section_keyword: Option<ColorSpec>,
    conditional_keyword: Option<ColorSpec>,
    macro_def_keyword: Option<ColorSpec>,
    macro_ref: Option<ColorSpec>,
    shell_macro: Option<ColorSpec>,
    expr_macro: Option<ColorSpec>,
    string: Option<ColorSpec>,
    number: Option<ColorSpec>,
    operator: Option<ColorSpec>,
    comment: Option<ColorSpec>,
    changelog_header: Option<ColorSpec>,
    shell_body: Option<ColorSpec>,
    text_body: Option<ColorSpec>,
    flag: Option<ColorSpec>,
}

impl Theme {
    /// Built-in dark theme — readable on a terminal with dark
    /// background and the standard 16-colour palette.
    pub fn dark() -> Self {
        Self {
            tag_name: Some(bold(Color::Blue)),
            tag_qualifier: Some(fg(Color::Cyan)),
            section_keyword: Some(bold(Color::Magenta)),
            conditional_keyword: Some(bold(Color::Magenta)),
            macro_def_keyword: Some(bold(Color::Magenta)),
            macro_ref: Some(fg(Color::Cyan)),
            shell_macro: Some(italic(fg(Color::Cyan))),
            expr_macro: Some(fg(Color::Cyan)),
            string: Some(fg(Color::Green)),
            number: Some(fg(Color::Yellow)),
            operator: Some(fg(GREY)),
            comment: Some(italic(fg(GREY))),
            changelog_header: Some(bold(Color::Yellow)),
            // ShellBody / TextBody stay None — default terminal fg.
            shell_body: None,
            text_body: None,
            flag: Some(fg(Color::Cyan)),
        }
    }

    fn spec_for(&self, kind: TokenKind) -> Option<&ColorSpec> {
        match kind {
            TokenKind::TagName => self.tag_name.as_ref(),
            TokenKind::TagQualifier => self.tag_qualifier.as_ref(),
            TokenKind::SectionKeyword => self.section_keyword.as_ref(),
            TokenKind::ConditionalKeyword => self.conditional_keyword.as_ref(),
            TokenKind::MacroDefKeyword => self.macro_def_keyword.as_ref(),
            TokenKind::MacroRef => self.macro_ref.as_ref(),
            TokenKind::ShellMacro => self.shell_macro.as_ref(),
            TokenKind::ExprMacro => self.expr_macro.as_ref(),
            TokenKind::String => self.string.as_ref(),
            TokenKind::Number => self.number.as_ref(),
            TokenKind::Operator => self.operator.as_ref(),
            TokenKind::Comment => self.comment.as_ref(),
            TokenKind::ChangelogHeader => self.changelog_header.as_ref(),
            TokenKind::ShellBody => self.shell_body.as_ref(),
            TokenKind::TextBody => self.text_body.as_ref(),
            TokenKind::Flag => self.flag.as_ref(),
            TokenKind::Plain => None,
            // `TokenKind` is `#[non_exhaustive]` — future variants
            // default to plain rendering until a theme entry is added.
            _ => None,
        }
    }
}

fn fg(c: Color) -> ColorSpec {
    let mut s = ColorSpec::new();
    s.set_fg(Some(c));
    s
}

fn bold(c: Color) -> ColorSpec {
    let mut s = ColorSpec::new();
    s.set_fg(Some(c)).set_bold(true);
    s
}

fn italic(mut s: ColorSpec) -> ColorSpec {
    s.set_italic(true);
    s
}

/// Adapter wrapping any [`WriteColor`] sink as a [`PrintWriter`].
/// Each emitted chunk is wrapped in `set_color(spec_for(kind))` /
/// `reset()`, ensuring colour state never leaks past a token boundary.
///
/// The [`PrintWriter::emit`] signature returns `()` (upstream contract
/// in the `rpm_spec` crate), so I/O failures cannot propagate out of
/// the call directly. Instead the first non-`BrokenPipe` error is
/// latched in `io_error` and the caller drains it via
/// [`AnsiWriter::take_error`] after the printer has finished, mapping
/// it to a non-zero exit code. `BrokenPipe` is treated as normal
/// downstream termination and ignored — see
/// `commands::pretty::Cmd::run` for the matching short-circuit.
pub struct AnsiWriter<'a, W: WriteColor> {
    out: &'a mut W,
    theme: Theme,
    io_error: Option<io::Error>,
}

impl<'a, W: WriteColor> AnsiWriter<'a, W> {
    pub fn new(out: &'a mut W, theme: Theme) -> Self {
        Self {
            out,
            theme,
            io_error: None,
        }
    }

    /// Drain the first latched I/O error, if any. `BrokenPipe` errors
    /// are never latched — they are a normal termination signal when
    /// the downstream consumer (e.g. `| head`) closes early.
    pub fn take_error(&mut self) -> Option<io::Error> {
        self.io_error.take()
    }

    /// Latch `err` if it is the first non-`BrokenPipe` failure. A
    /// `BrokenPipe` short-circuits all subsequent emits via the
    /// caller's `has_broken_pipe` check (see `commands::pretty`), so
    /// we record it here too — but as a sentinel the caller treats as
    /// SUCCESS, not error.
    fn record(&mut self, err: io::Error) {
        if self.io_error.is_none() {
            self.io_error = Some(err);
        }
    }

    /// `true` once any emit has observed `ErrorKind::BrokenPipe`.
    /// Callers use this to stop feeding the printer when the reader
    /// went away — there is no point tokenising the rest of the AST.
    pub fn has_broken_pipe(&self) -> bool {
        self.io_error
            .as_ref()
            .map(|e| e.kind() == io::ErrorKind::BrokenPipe)
            .unwrap_or(false)
    }
}

impl<W: WriteColor> PrintWriter for AnsiWriter<'_, W> {
    fn emit(&mut self, kind: TokenKind, text: &str) {
        if text.is_empty() {
            return;
        }
        // Once any byte has hit a broken pipe, further writes are
        // pointless and would just re-trigger EPIPE on every chunk.
        if self.has_broken_pipe() {
            return;
        }
        // `PrintWriter::emit` returns `()`, so we cannot bubble I/O
        // errors through the printer. Latch the first failure for the
        // caller to inspect via `take_error`; `BrokenPipe` is recorded
        // separately as a normal-termination signal.
        if let Some(spec) = self.theme.spec_for(kind) {
            if let Err(e) = self.out.set_color(spec) {
                self.record(e);
                return;
            }
            if let Err(e) = self.out.write_all(text.as_bytes()) {
                self.record(e);
                return;
            }
            if let Err(e) = self.out.reset() {
                self.record(e);
            }
        } else if let Err(e) = self.out.write_all(text.as_bytes()) {
            self.record(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespan_reporting::term::termcolor::{Ansi, NoColor};

    fn render_with<W>(out: &mut W, text: &str, kind: TokenKind, theme: Theme)
    where
        W: WriteColor,
    {
        let mut w = AnsiWriter::new(out, theme);
        w.emit(kind, text);
    }

    #[test]
    fn plain_kind_emits_no_ansi_escape() {
        let mut buf = Vec::new();
        {
            let mut ansi = Ansi::new(&mut buf);
            render_with(&mut ansi, "hello", TokenKind::Plain, Theme::dark());
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains('\x1b'), "no ANSI escape expected: {s:?}");
        assert_eq!(s, "hello");
    }

    #[test]
    fn coloured_kind_wraps_text_in_ansi_escapes() {
        let mut buf = Vec::new();
        {
            let mut ansi = Ansi::new(&mut buf);
            render_with(&mut ansi, "Name", TokenKind::TagName, Theme::dark());
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains('\x1b'), "ANSI escape expected: {s:?}");
        assert!(s.contains("Name"));
        // Reset sequence must follow — no colour bleed.
        assert!(s.ends_with("\x1b[0m"), "expected ANSI reset, got: {s:?}");
    }

    #[test]
    fn no_color_sink_passes_through_verbatim() {
        let mut buf = Vec::new();
        {
            let mut sink = NoColor::new(&mut buf);
            render_with(&mut sink, "Version", TokenKind::TagName, Theme::dark());
        }
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "Version", "NoColor sink must not inject escapes");
    }

    #[test]
    fn empty_text_emits_nothing() {
        let mut buf = Vec::new();
        {
            let mut ansi = Ansi::new(&mut buf);
            render_with(&mut ansi, "", TokenKind::TagName, Theme::dark());
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn multiple_tokens_isolate_color_state() {
        // Sanity guard against "colour bleed": after each coloured
        // token the second token's escapes must not be a continuation
        // of the first. We rely on the fact that termcolor emits a
        // reset every time `set_color`/`reset` is called, so the
        // total reset count is strictly ≥ once per coloured token.
        // Exact escape sequences differ between termcolor versions —
        // assert the substring invariant only.
        let mut buf = Vec::new();
        let theme = Theme::dark();
        {
            let mut ansi = Ansi::new(&mut buf);
            let mut w = AnsiWriter::new(&mut ansi, theme);
            w.emit(TokenKind::TagName, "Name");
            w.emit(TokenKind::Plain, ": ");
            w.emit(TokenKind::String, "\"hi\"");
        }
        let s = String::from_utf8(buf).unwrap();
        let reset_count = s.matches("\x1b[0m").count();
        assert!(
            reset_count >= 2,
            "expected ≥2 resets, got {reset_count}: {s:?}"
        );
        // Output must end with a reset, otherwise the trailing colour
        // would bleed past the printer's last byte.
        assert!(s.ends_with("\x1b[0m"), "expected trailing reset: {s:?}");
    }
}
