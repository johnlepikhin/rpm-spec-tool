//! Phase 3 changelog-health lints (RPM037, RPM038, RPM039).
//!
//! All three operate on `ChangelogEntry` headers / bodies and live in
//! one file because they share a single `visit_changelog_entry` shape
//! and reuse `current_year()` between them.
//!
//! - **RPM037 `empty-changelog-entry`** — entries with no meaningful
//!   body line. A changelog whose body is just whitespace is a typo or
//!   leftover from a copy-paste.
//! - **RPM038 `changelog-future-date`** — entries dated **after**
//!   today. Usually a typo (`2025` instead of `2024`) but occasionally
//!   intentional for embargoed releases.
//! - **RPM039 `changelog-implausible-date`** — entries with day or year
//!   outside any reasonable range. Catches `day=99`, `year=1500`, etc.
//!   The rpm-spec parser also emits these as `rpmspec/W0025`; we
//!   re-detect from the AST so the diagnostic is surfaced through the
//!   regular lint pipeline (with severity overrides, JSON output, ...).
//!
//! All three are `LintCategory::Correctness`.

use rpm_spec::ast::{ChangelogEntry, Span, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

// ---------------------------------------------------------------------
// RPM037 empty-changelog-entry
// ---------------------------------------------------------------------

/// Lint metadata for RPM037 `empty-changelog-entry`.
pub static EMPTY_ENTRY_METADATA: LintMetadata = LintMetadata {
    id: "RPM037",
    name: "empty-changelog-entry",
    description: "Changelog entry has no body text — likely a leftover header.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct EmptyChangelogEntry {
    diagnostics: Vec<Diagnostic>,
}

impl EmptyChangelogEntry {
    pub fn new() -> Self {
        Self::default()
    }
}

fn entry_body_is_empty(entry: &ChangelogEntry<Span>) -> bool {
    entry.body.iter().all(|line| {
        line.segments.iter().all(|seg| match seg {
            TextSegment::Literal(s) => s.trim().is_empty(),
            TextSegment::Macro(_) => false,
            _ => true,
        })
    })
}

impl<'ast> Visit<'ast> for EmptyChangelogEntry {
    fn visit_changelog_entry(&mut self, node: &'ast ChangelogEntry<Span>) {
        if entry_body_is_empty(node) {
            self.diagnostics.push(Diagnostic::new(
                &EMPTY_ENTRY_METADATA,
                Severity::Warn,
                "changelog entry body is empty",
                node.data,
            ));
        }
    }
}

impl Lint for EmptyChangelogEntry {
    fn metadata(&self) -> &'static LintMetadata {
        &EMPTY_ENTRY_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// ---------------------------------------------------------------------
// RPM038 changelog-future-date
// ---------------------------------------------------------------------

/// Lint metadata for RPM038 `changelog-future-date`.
pub static FUTURE_DATE_METADATA: LintMetadata = LintMetadata {
    id: "RPM038",
    name: "changelog-future-date",
    description: "Changelog entry is dated in the future.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug)]
pub struct ChangelogFutureDate {
    diagnostics: Vec<Diagnostic>,
    current_year: i32,
}

impl Default for ChangelogFutureDate {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            current_year: current_year_utc(),
        }
    }
}

impl ChangelogFutureDate {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ChangelogFutureDate {
    fn visit_changelog_entry(&mut self, node: &'ast ChangelogEntry<Span>) {
        if i32::from(node.date.year) > self.current_year {
            self.diagnostics.push(Diagnostic::new(
                &FUTURE_DATE_METADATA,
                Severity::Warn,
                format!(
                    "changelog entry dated {} — future entries are usually typos",
                    node.date.year
                ),
                node.data,
            ));
        }
    }
}

impl Lint for ChangelogFutureDate {
    fn metadata(&self) -> &'static LintMetadata {
        &FUTURE_DATE_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// ---------------------------------------------------------------------
// RPM039 changelog-implausible-date
// ---------------------------------------------------------------------

/// Lint metadata for RPM039 `changelog-implausible-date`.
pub static IMPLAUSIBLE_DATE_METADATA: LintMetadata = LintMetadata {
    id: "RPM039",
    name: "changelog-implausible-date",
    description: "Changelog entry date has an impossible day or year.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Earliest year we accept. RPM project was started in 1997; anything
/// markedly older in a `%changelog` is virtually always a typo
/// (`year=98` parsed as 98 vs. 1998).
const MIN_PLAUSIBLE_YEAR: u16 = 1990;

/// Years past the current one that we still allow without flagging
/// "too far in the future". `RPM038 changelog-future-date` already
/// fires for `year > current`; this margin avoids RPM039 dogpiling on
/// the same entry for trivial typos (`2026` written in 2025) — the
/// "implausible" tier kicks in only when the year is plausibly *wrong*,
/// not just *future*.
const RPM039_FUTURE_GRACE_YEARS: i32 = 5;

#[derive(Debug)]
pub struct ChangelogImplausibleDate {
    diagnostics: Vec<Diagnostic>,
    current_year: i32,
}

impl Default for ChangelogImplausibleDate {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            current_year: current_year_utc(),
        }
    }
}

impl ChangelogImplausibleDate {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ChangelogImplausibleDate {
    fn visit_changelog_entry(&mut self, node: &'ast ChangelogEntry<Span>) {
        let day = node.date.day;
        let year = node.date.year;
        // `time::Date::year()` is bounded by ±9999, so `current_year`
        // always fits in u16, but the additive grace still gets
        // protected against weird system clocks via saturating + try_from.
        let upper_year: u16 = u16::try_from(
            self.current_year.saturating_add(RPM039_FUTURE_GRACE_YEARS),
        )
        .unwrap_or(u16::MAX);

        // TODO: a calendar-aware day check (Feb 30, Apr 31, leap-year
        // handling) would catch more typos. For now we use the coarse
        // 1..=31 bound — the parser already rejects mass garbage like
        // `Mon Foo 99 2024`.
        let reason = if !(1..=31).contains(&day) {
            Some(format!("day-of-month `{day}` is outside 1..=31"))
        } else if year < MIN_PLAUSIBLE_YEAR {
            Some(format!(
                "year `{year}` is before {MIN_PLAUSIBLE_YEAR} — likely a two-digit typo"
            ))
        } else if year > upper_year {
            // RPM038 fires for any `year > current` (likely typo).
            // RPM039 only steps in when the year is far enough out to
            // look like data corruption rather than a one-digit slip.
            // The grace window (`RPM039_FUTURE_GRACE_YEARS`) keeps the
            // two rules from double-firing on common cases like writing
            // the next year's date by accident.
            Some(format!(
                "year `{year}` is too far in the future (current is {current})",
                current = self.current_year
            ))
        } else {
            None
        };

        if let Some(reason) = reason {
            self.diagnostics.push(Diagnostic::new(
                &IMPLAUSIBLE_DATE_METADATA,
                Severity::Warn,
                format!("implausible changelog date: {reason}"),
                node.data,
            ));
        }
    }
}

impl Lint for ChangelogImplausibleDate {
    fn metadata(&self) -> &'static LintMetadata {
        &IMPLAUSIBLE_DATE_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// Current UTC year. Sourced from the `time` crate so leap-second and
/// timezone math stays correct.
fn current_year_utc() -> i32 {
    time::OffsetDateTime::now_utc().year()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_empty(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = EmptyChangelogEntry::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_future(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ChangelogFutureDate::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_implausible(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ChangelogImplausibleDate::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM037 -----

    #[test]
    fn empty_flags_blank_body() {
        let src = "Name: x\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n\n";
        let diags = run_empty(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM037");
    }

    #[test]
    fn empty_silent_for_real_body() {
        let src = "Name: x\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- something\n";
        assert!(run_empty(src).is_empty());
    }

    // ----- RPM038 -----

    #[test]
    fn future_flags_year_3000() {
        let src = "Name: x\n%changelog\n* Mon Jan 01 3000 a <a@b> - 1-1\n- init\n";
        let diags = run_future(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM038");
    }

    #[test]
    fn future_silent_for_past_year() {
        let src = "Name: x\n%changelog\n* Mon Jan 01 2020 a <a@b> - 1-1\n- init\n";
        assert!(run_future(src).is_empty());
    }

    // ----- RPM039 -----

    #[test]
    fn implausible_flags_old_year() {
        let src = "Name: x\n%changelog\n* Mon Jan 01 1500 a <a@b> - 1-1\n- init\n";
        let diags = run_implausible(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM039");
        assert!(diags[0].message.contains("1500"));
    }

    #[test]
    fn implausible_silent_for_normal_date() {
        let src = "Name: x\n%changelog\n* Mon Jan 15 2024 a <a@b> - 1-1\n- init\n";
        assert!(run_implausible(src).is_empty());
    }

    #[test]
    fn current_year_is_reasonable() {
        // Cheap sanity that the `time` crate gives us a recent year —
        // protects against pre-2000 / way-in-future clocks in CI.
        let y = current_year_utc();
        assert!(
            (2020..=2100).contains(&y),
            "current_year_utc returned {y}, system clock looks broken"
        );
    }
}
