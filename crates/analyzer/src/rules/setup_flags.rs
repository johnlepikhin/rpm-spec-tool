//! RPM063 `setup-without-q-flag` — flag `%setup` invocations that
//! don't pass `-q` (quiet). The flag suppresses verbose tarball
//! extraction output; idiomatic spec files always use it.
//!
//! Matches:
//! - `%setup -q` — quiet, single flag (silent).
//! - `%setup -qn foo` / `%setup -nq foo` — combined short flags
//!   containing `q` (silent).
//! - `%setup --quiet` — long form (silent).
//! - `%setup -n foo` — flagged: no quiet.
//!
//! Conservative fallback: if any arg is non-literal (contains a macro
//! expansion), we can't statically tell what flags expand to, so the
//! rule stays silent — better than a false positive on `%setup %{?my_opts}`.

use rpm_spec::ast::{BuildScriptKind, Section, Span, Text, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::MACRO_SETUP;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM063",
    name: "setup-without-q-flag",
    description: "`%setup` should always be invoked with `-q` to silence tarball extraction noise.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct SetupWithoutQFlag {
    diagnostics: Vec<Diagnostic>,
    /// Span of the enclosing `%prep` section body. `MacroRef` doesn't
    /// carry a span (the AST stores macros inside `TextSegment::Macro`
    /// which lacks per-segment offsets), so we anchor diagnostics on
    /// the section that holds the `%setup` call. Only set while inside
    /// `%prep` — `%setup` outside `%prep` is a different category of
    /// bug and not this rule's business.
    current_prep_span: Option<Span>,
}

impl SetupWithoutQFlag {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SetupWithoutQFlag {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        let prev = self.current_prep_span.take();
        if let Section::BuildScript { kind: BuildScriptKind::Prep, data, .. } = node {
            self.current_prep_span = Some(*data);
        }
        visit::walk_section(self, node);
        self.current_prep_span = prev;
    }

    fn visit_text(&mut self, node: &'ast Text) {
        // `%setup` is a `Plain` macro: the rpm-spec parser produces
        // `MacroRef { name: "setup", args: [] }` and leaves the
        // arguments as a `Literal(" -q -n foo")` segment **immediately
        // following** the macro in the same `Text` line. Scan
        // post-`%setup` siblings rather than `MacroRef::args`.
        let Some(anchor) = self.current_prep_span else {
            return;
        };
        for (i, seg) in node.segments.iter().enumerate() {
            let TextSegment::Macro(m) = seg else { continue };
            if m.name != MACRO_SETUP {
                continue;
            }
            if has_quiet_in_trailing_args(&node.segments[i + 1..]) {
                continue;
            }
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "`%setup` invoked without `-q`",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "add `-q` to silence tarball extraction output",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
        visit::walk_text(self, node);
    }
}

impl Lint for SetupWithoutQFlag {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

/// Scan the `TextSegment`s that follow `%setup` on the same line for a
/// quiet flag.
///
/// Returns `true` (suppresses the lint) when:
/// - a whitespace-delimited token like `-q`, `-qn`, `--quiet`, `-Tqc`,
///   … is found in the trailing literals, **or**
/// - a macro appears in the trailing segments (could expand to `-q`;
///   bail out conservatively rather than false-flag).
///
/// `--quiet` is matched as a whole token; `--quiet-mode` does not
/// match because the long-flag form has no `q` rule.
fn has_quiet_in_trailing_args(trailing: &[TextSegment]) -> bool {
    let mut accumulated = String::new();
    for seg in trailing {
        match seg {
            TextSegment::Literal(s) => accumulated.push_str(s),
            TextSegment::Macro(_) => return true,
            // `TextSegment` is `#[non_exhaustive]`; any future variant
            // is treated like a macro — unknown content, bail out.
            _ => return true,
        }
    }
    accumulated.split_ascii_whitespace().any(is_quiet_flag)
}

fn is_quiet_flag(s: &str) -> bool {
    if s == "--quiet" {
        return true;
    }
    // Combined short flag: `-q`, `-qn`, `-nq`, `-Tcq`, ... anything
    // that starts with a single `-` and contains a `q` after.
    if let Some(rest) = s.strip_prefix('-')
        && !rest.starts_with('-')
        && rest.contains('q')
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SetupWithoutQFlag::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_setup_without_q() {
        let src = "Name: x\n%prep\n%setup -n foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM063");
    }

    #[test]
    fn flags_bare_setup() {
        let src = "Name: x\n%prep\n%setup\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_setup_q() {
        let src = "Name: x\n%prep\n%setup -q\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_setup_q_with_name() {
        let src = "Name: x\n%prep\n%setup -q -n foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_setup_qn_combined() {
        // `-qn foo` — combined short flags, quiet is included.
        let src = "Name: x\n%prep\n%setup -qn foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_setup_nq_combined() {
        // Same flags in reverse order.
        let src = "Name: x\n%prep\n%setup -nq foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_long_quiet() {
        let src = "Name: x\n%prep\n%setup --quiet -n foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_autosetup() {
        // `%autosetup` is a different macro — RPM063 only watches `%setup`.
        let src = "Name: x\n%prep\n%autosetup -n foo -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_setup_outside_prep() {
        // `%setup` in `%build` is a different category of bug (not
        // ours to flag). The rule only watches the `%prep` body.
        let src = "Name: x\n%build\n%setup -n foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_arg_contains_macro() {
        // Conservative bail-out: `%{my_opts}` could expand to `-q`,
        // so we don't warn.
        let src = "Name: x\n%prep\n%setup %{my_opts}\n";
        assert!(run(src).is_empty());
    }
}
