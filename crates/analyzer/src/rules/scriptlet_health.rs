//! Scriptlet hygiene rules: RPM340, RPM341.
//!
//! - **RPM340 `scriptlet-exit-not-guaranteed-zero`** — RPM aborts the
//!   install/upgrade/erase transaction when a scriptlet exits with a
//!   non-zero status. Bodies that end on a command that *can* fail
//!   (without `|| :` / `|| true` / an explicit `exit 0`), or that
//!   carry `set -e` plus a fallible call, leave the package in a
//!   half-installed state. Fedora packaging guidelines spell this
//!   out as a hard rule. We flag scriptlets where the last
//!   meaningful line is a bare command without an exit guard and
//!   no explicit `exit 0` follows.
//! - **RPM341 `scriptlet-upgrade-test-eq-two`** — the install count
//!   `$1` is `1` on first install and `2+` on upgrades — but only in
//!   the **simple** case. Multilib, error recovery, and overlap with
//!   other packages can push the value beyond 2. Comparing exactly to
//!   `2` (`$1 = 2`, `-eq 2`, `== 2`) silently breaks those edge
//!   cases. Use `[ $1 -gt 1 ]` instead.

use rpm_spec::ast::{Scriptlet, Span, SpecFile, Text, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::for_each_scriptlet;
use crate::visit::Visit;

// =====================================================================
// RPM340 scriptlet-exit-not-guaranteed-zero
// =====================================================================

pub static EXIT_GUARDED_METADATA: LintMetadata = LintMetadata {
    id: "RPM340",
    name: "scriptlet-exit-not-guaranteed-zero",
    description: "A scriptlet's last command can fail with no explicit exit guard. RPM aborts \
                  the transaction on non-zero exit, leaving the system half-installed. Add \
                  `|| :` / `|| true` / `exit 0`, or use `set +e`.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct ScriptletExitNotGuaranteedZero {
    diagnostics: Vec<Diagnostic>,
}

impl ScriptletExitNotGuaranteedZero {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ScriptletExitNotGuaranteedZero {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for_each_scriptlet(spec, |s| {
            if scriptlet_is_lua(s) {
                return;
            }
            // Find the index of the last *meaningful* line (non-blank,
            // non-comment).
            let Some(last_idx) = last_meaningful_line(&s.body.lines) else {
                return;
            };
            let last = &s.body.lines[last_idx];
            if line_guarantees_zero_exit(last) {
                return;
            }
            // An explicit `exit 0` *anywhere* after the last fallible
            // command is enough — but the last-line check has already
            // covered that case (a final `exit 0` is itself a guard).
            // What we additionally accept: every line preceded by
            // `set +e` and no `set -e` flips it back on.
            if has_set_minus_e_active_at_last(&s.body.lines, last_idx) {
                self.diagnostics.push(diag_340(s.data));
                return;
            }
            // No `set -e`: a bare last line that *can* fail is the
            // problem. Conservatively flag whenever the last line
            // looks command-like (has any literal, non-keyword token)
            // and has no `|| :` / `|| true` / `exit 0` guard.
            if line_is_potentially_fallible(last) {
                self.diagnostics.push(diag_340(s.data));
            }
        });
    }
}

fn diag_340(span: Span) -> Diagnostic {
    Diagnostic::new(
        &EXIT_GUARDED_METADATA,
        Severity::Warn,
        "scriptlet's last command has no exit-zero guard; append `|| :`, `|| true`, or \
         `exit 0` so RPM does not abort the transaction on failure",
        span,
    )
}

fn scriptlet_is_lua(s: &Scriptlet<Span>) -> bool {
    matches!(s.interp, Some(rpm_spec::ast::Interpreter::Lua))
}

fn last_meaningful_line(lines: &[Text]) -> Option<usize> {
    for (i, line) in lines.iter().enumerate().rev() {
        if !is_blank_or_comment(line) {
            return Some(i);
        }
    }
    None
}

fn is_blank_or_comment(line: &Text) -> bool {
    // Comment: literal line where the first non-whitespace character
    // is `#`. A line with macros is conservatively *not* a comment.
    if line.segments.is_empty() {
        return true;
    }
    let Some(lit) = line.literal_str() else {
        return false;
    };
    let trimmed = lit.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn line_guarantees_zero_exit(line: &Text) -> bool {
    let Some(lit) = line.literal_str() else {
        return false;
    };
    let trimmed = lit.trim();
    // `:` is the no-op builtin that always succeeds; `true` likewise.
    if trimmed == ":" || trimmed == "true" || trimmed == "exit 0" {
        return true;
    }
    // Trailing `|| :`, `|| true`, `|| exit 0`. We do a coarse `ends_with`
    // tolerant of whitespace.
    let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.ends_with("|| :")
        || collapsed.ends_with("|| true")
        || collapsed.ends_with("|| exit 0")
}

fn line_is_potentially_fallible(line: &Text) -> bool {
    // A line with a macro reference is treated as potentially
    // fallible — we can't tell what it expands to. Pure-whitespace
    // and comments were filtered out by `last_meaningful_line`.
    !is_blank_or_comment(line)
}

fn has_set_minus_e_active_at_last(lines: &[Text], last_idx: usize) -> bool {
    let mut active = false;
    for line in &lines[..=last_idx] {
        let Some(lit) = line.literal_str() else {
            continue;
        };
        let trimmed = lit.trim();
        // Match `set -e` and `set -eu`/`set -euo pipefail` etc.
        if trimmed == "set -e"
            || trimmed.starts_with("set -e ")
            || trimmed.starts_with("set -eu")
            || trimmed.starts_with("set -eo")
            || trimmed.starts_with("set -euo")
        {
            active = true;
        } else if trimmed == "set +e" || trimmed.starts_with("set +e ") {
            active = false;
        }
    }
    active
}

impl Lint for ScriptletExitNotGuaranteedZero {
    fn metadata(&self) -> &'static LintMetadata {
        &EXIT_GUARDED_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM341 scriptlet-upgrade-test-eq-two
// =====================================================================

pub static UPGRADE_TEST_METADATA: LintMetadata = LintMetadata {
    id: "RPM341",
    name: "scriptlet-upgrade-test-eq-two",
    description: "Scriptlet compares the install count `$1` to exactly `2` to detect an \
                  upgrade. Multilib and error recovery can push `$1` above `2`; use \
                  `[ $1 -gt 1 ]` instead.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct ScriptletUpgradeTestEqTwo {
    diagnostics: Vec<Diagnostic>,
}

impl ScriptletUpgradeTestEqTwo {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ScriptletUpgradeTestEqTwo {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for_each_scriptlet(spec, |s| {
            if scriptlet_is_lua(s) {
                return;
            }
            if s.body.lines.iter().any(line_matches_eq_two) {
                self.diagnostics.push(Diagnostic::new(
                    &UPGRADE_TEST_METADATA,
                    Severity::Warn,
                    "scriptlet compares `$1` to literal `2`; multilib/error recovery can make \
                     `$1 > 2` — use `[ $1 -gt 1 ]` to mean \"this is an upgrade\"",
                    s.data,
                ));
            }
        });
    }
}

fn line_matches_eq_two(line: &Text) -> bool {
    // We only need the literal text — macro-only lines can't form
    // these test idioms.
    let mut lit = String::new();
    for seg in &line.segments {
        if let TextSegment::Literal(s) = seg {
            lit.push_str(s);
        }
    }
    let normalised: String = lit.split_whitespace().collect::<Vec<_>>().join(" ");

    // Idioms we flag:
    //   if [ "$1" = 2 ]              if [ $1 = 2 ]
    //   if [ "$1" -eq 2 ]            if [ $1 -eq 2 ]
    //   if [ "$1" == 2 ]             if [[ $1 == 2 ]]
    //   test "$1" -eq 2              [[ $1 -eq 2 ]]
    contains_dollar_one_eq_two(&normalised)
}

fn contains_dollar_one_eq_two(s: &str) -> bool {
    // Look for `$1` followed by ` = 2`, ` -eq 2`, or ` == 2`, allowing
    // the optional double-quote pair around `$1`.
    let needles_after = [" = 2", " -eq 2", " == 2"];
    for op in &needles_after {
        for prefix in ["$1", "\"$1\""] {
            let mut idx = 0;
            while let Some(found) = s[idx..].find(prefix) {
                let after_prefix_start = idx + found + prefix.len();
                if s[after_prefix_start..].starts_with(op) {
                    // Verify the next char after `2` is a boundary
                    // (whitespace, `]`, `)`, end-of-string) — avoid
                    // matching `... -eq 20`.
                    let end_idx = after_prefix_start + op.len();
                    match s.as_bytes().get(end_idx) {
                        None => return true,
                        Some(b) if !b.is_ascii_digit() => return true,
                        Some(_) => {}
                    }
                }
                idx = after_prefix_start;
            }
        }
    }
    false
}

impl Lint for ScriptletUpgradeTestEqTwo {
    fn metadata(&self) -> &'static LintMetadata {
        &UPGRADE_TEST_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_340(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ScriptletExitNotGuaranteedZero::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_341(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ScriptletUpgradeTestEqTwo::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM340 -----

    #[test]
    fn rpm340_flags_bare_failing_last_command() {
        let src = "Name: x\n%post\nrm -f /tmp/foo.lock\n";
        let diags = run_340(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM340");
    }

    #[test]
    fn rpm340_silent_with_exit_zero() {
        let src = "Name: x\n%post\nsystemctl restart foo\nexit 0\n";
        assert!(run_340(src).is_empty());
    }

    #[test]
    fn rpm340_silent_with_or_colon_guard() {
        let src = "Name: x\n%post\nsystemctl restart foo || :\n";
        assert!(run_340(src).is_empty());
    }

    #[test]
    fn rpm340_silent_with_or_true_guard() {
        let src = "Name: x\n%post\nsystemctl restart foo || true\n";
        assert!(run_340(src).is_empty());
    }

    #[test]
    fn rpm340_ignores_trailing_blank_lines_and_comments() {
        // The actual last meaningful line is `exit 0` — blank line
        // and comment after it should not change the verdict.
        let src = "Name: x\n%post\nsystemctl restart foo\nexit 0\n\n# done\n";
        assert!(run_340(src).is_empty());
    }

    #[test]
    fn rpm340_silent_for_lua_interpreter() {
        // Lua scriptlets don't observe shell exit-status semantics.
        let src = "Name: x\n%post -p <lua>\nprint(\"hi\")\n";
        assert!(run_340(src).is_empty());
    }

    // ----- RPM341 -----

    #[test]
    fn rpm341_flags_dollar_one_eq_two_with_test() {
        let src = "Name: x\n%post\nif [ \"$1\" = 2 ]; then echo up; fi\nexit 0\n";
        let diags = run_341(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM341");
    }

    #[test]
    fn rpm341_flags_dollar_one_dash_eq_two() {
        let src = "Name: x\n%postun\nif [ $1 -eq 2 ]; then echo up; fi\nexit 0\n";
        assert_eq!(run_341(src).len(), 1);
    }

    #[test]
    fn rpm341_flags_double_eq_two() {
        let src = "Name: x\n%post\nif [[ $1 == 2 ]]; then echo up; fi\nexit 0\n";
        assert_eq!(run_341(src).len(), 1);
    }

    #[test]
    fn rpm341_silent_with_gt_one() {
        let src = "Name: x\n%post\nif [ $1 -gt 1 ]; then echo up; fi\nexit 0\n";
        assert!(run_341(src).is_empty());
    }

    #[test]
    fn rpm341_silent_for_eq_20_boundary() {
        // `$1 -eq 20` shouldn't fire just because the prefix matches.
        let src = "Name: x\n%post\nif [ $1 -eq 20 ]; then echo many; fi\nexit 0\n";
        assert!(run_341(src).is_empty());
    }

    #[test]
    fn rpm341_silent_for_lua() {
        let src = "Name: x\n%post -p <lua>\nif $1 == 2 then end\n";
        assert!(run_341(src).is_empty());
    }
}
