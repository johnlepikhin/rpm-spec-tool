//! RPM386 `werror-not-disabled` — `%build` passes `-Werror` (or
//! `--enable-werror` / `--enable-werror=yes` / `--with-warnings=error`)
//! to a configure or compile invocation.
//!
//! Treating every warning as an error is fine in upstream CI but
//! catastrophic in a downstream packaging context: when GCC/Clang ship
//! a new warning (or sharpen an existing one), every spec built on the
//! new compiler suddenly fails. Distributions therefore ask packagers
//! to *not* propagate `-Werror` from upstream's default; the recipe is
//! typically a `sed`/`configure --disable-werror`/`make … WERROR_FLAG=`
//! line earlier in `%build`.
//!
//! The rule fires on a bare `-Werror` token. Scoped forms
//! (`-Werror=foo` / `-Wno-error=foo`) opt into or out of one specific
//! warning and are intentional — those are not flagged. Likewise,
//! `-Werror=implicit-function-declaration` is benign and common.
//!
//! Only `%build` is inspected — `-Werror` in `%install`/`%check` is
//! unusual but the rule walks every build-script section for
//! coverage.

use rpm_spec::ast::{ShellBody, Span, SpecFile, Text};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::{for_each_buildscript, strip_trailing_comment};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM386",
    name: "werror-not-disabled",
    description: "Build script passes `-Werror` / `--enable-werror`. New compiler versions add \
                  warnings that then break the build; disable `-Werror` for downstream packaging.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Affirmative values recognised after `--enable-werror=`. Anything
/// outside this set (notably `no`, `false`, `0`) is treated as the
/// disable form and does not fire the rule.
const AFFIRMATIVE_VALUES: &[&str] = &["yes", "true", "1"];

/// RPM386 — flags `-Werror` / `--enable-werror` left enabled in
/// `%build` / `%install` / `%check` shell bodies.
#[derive(Debug, Default)]
pub struct WerrorNotDisabled {
    diagnostics: Vec<Diagnostic>,
}

impl WerrorNotDisabled {
    /// Construct a fresh lint instance with no diagnostics buffered.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for WerrorNotDisabled {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for_each_buildscript(spec, |_kind, body, section_span| {
            scan_body(body, section_span, &mut self.diagnostics);
        });
    }
}

fn scan_body(body: &ShellBody, section_span: Span, out: &mut Vec<Diagnostic>) {
    // Emit at most one diagnostic per section — a `%build` that says
    // `-Werror` on five lines is one problem, not five.
    for line in &body.lines {
        if !line_has_werror(line) {
            continue;
        }
        out.push(Diagnostic::new(
            &METADATA,
            Severity::Warn,
            "build script passes `-Werror` (or `--enable-werror`); new compilers will add \
             warnings that break the build — disable it for downstream packaging",
            section_span,
        ));
        return;
    }
}

/// `true` when `line`'s literal text references the unscoped `-Werror`
/// or affirmative `--enable-werror` switch.
///
/// We scan the line text directly (not via the shell tokenizer)
/// because the dangerous forms — `make CFLAGS="-O2 -Werror"`,
/// `CFLAGS=-Werror foo`, `./configure --enable-werror` — vary in how
/// the tokenizer groups them. A boundary-aware substring search keeps
/// the logic in one place and avoids token-grouping accidents.
///
/// Scoped forms (`-Werror=foo`, `-Wno-error`, `--enable-werror=no`)
/// are returned `false`.
fn line_has_werror(line: &Text) -> bool {
    let Some(lit) = line.literal_str() else {
        return false;
    };
    let scan = strip_trailing_comment(lit);
    has_unscoped_werror(scan) || has_affirmative_enable_werror(scan)
}

/// `true` when `-Werror` appears as a whole flag (boundary left and
/// right) and is **not** followed by `=`. The latter excludes the
/// scoped `-Werror=warning-name` form, which is intentional.
fn has_unscoped_werror(s: &str) -> bool {
    let needle = "-Werror";
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle.as_bytes() {
            let left_ok = i == 0 || !is_flag_char(bytes[i - 1]);
            let after = bytes.get(i + needle.len());
            let right_ok = match after {
                None => true,
                Some(b'=') => false,
                Some(b) => !is_flag_char(*b),
            };
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// `true` when `--enable-werror` appears as a flag and is either
/// bare or followed by an affirmative value. `=no` / `=false` / `=0`
/// don't count.
fn has_affirmative_enable_werror(s: &str) -> bool {
    let needle = "--enable-werror";
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle.as_bytes() {
            let left_ok = i == 0 || !is_flag_char(bytes[i - 1]);
            if left_ok && enable_werror_is_affirmative(s, i + needle.len()) {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Inspect what follows a `--enable-werror` occurrence at `after_pos`
/// and decide whether the flag is affirmative. End-of-input and any
/// non-flag separator (space, quote, `;`, etc.) both count as the
/// bare-affirmative form. An `=` introduces a value that must match
/// [`AFFIRMATIVE_VALUES`]; everything else (including continuation
/// into another flag-character) is a non-match.
fn enable_werror_is_affirmative(s: &str, after_pos: usize) -> bool {
    let bytes = s.as_bytes();
    match bytes.get(after_pos) {
        None => true,
        Some(b'=') => {
            // Parse value up to the next non-word char.
            let mut j = after_pos + 1;
            while j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let val = s[after_pos + 1..j].to_ascii_lowercase();
            AFFIRMATIVE_VALUES.contains(&val.as_str())
        }
        Some(b) if !is_flag_char(*b) => true,
        Some(_) => false,
    }
}

/// `true` when `b` continues a flag-name token (alphanumeric, `-`,
/// `_`). Used to test word boundaries around a flag match.
#[inline]
fn is_flag_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

impl Lint for WerrorNotDisabled {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = WerrorNotDisabled::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_bare_werror() {
        let src = "Name: x\n%build\nmake CFLAGS=\"-O2 -Werror\"\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM386");
    }

    #[test]
    fn flags_configure_enable_werror() {
        let src = "Name: x\n%build\n./configure --enable-werror\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_configure_enable_werror_yes() {
        let src = "Name: x\n%build\n./configure --enable-werror=yes\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_configure_enable_werror_no() {
        // `--enable-werror=no` is the *fix*, not the smell.
        let src = "Name: x\n%build\n./configure --enable-werror=no\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_scoped_werror_eq() {
        // `-Werror=implicit-function-declaration` is intentional.
        let src = "Name: x\n%build\nmake CFLAGS=\"-Werror=implicit-function-declaration\"\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_wno_error() {
        let src = "Name: x\n%build\nmake CFLAGS=\"-Wno-error\"\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_normal_build() {
        let src = "Name: x\n%build\n%configure\nmake %{?_smp_mflags}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn deduplicates_per_section() {
        // `%build` mentioning `-Werror` on multiple lines emits once.
        let src = "Name: x\n%build\nmake CFLAGS=-Werror foo\nmake CFLAGS=-Werror bar\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_werror_with_trailing_comment() {
        // Real `-Werror` use; the trailing `# FIXME` is just narration
        // and must not hide the flag from the lint.
        let src = "Name: x\n%build\nmake CFLAGS=-Werror # FIXME drop this\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM386");
    }

    #[test]
    fn silent_for_werror_inside_full_comment() {
        // Whole line is a comment — `strip_trailing_comment` reduces it
        // to empty/whitespace and the rule must stay silent.
        let src = "Name: x\n%build\n# discussion of -Werror\n";
        assert!(run(src).is_empty());
    }
}
