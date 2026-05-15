//! Apply lint suggestions back to source text.
//!
//! Strategy: gather every edit from every diagnostic for the current pass,
//! sort by descending `start_byte`, drop any edit whose range overlaps an
//! already-accepted one (MachineApplicable wins over MaybeIncorrect when
//! both touch the same bytes), apply, and re-parse. Repeat until a pass
//! produces no applicable edits or we hit a sanity cap.

use anyhow::Result;
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::{Applicability, Diagnostic, Edit, LintSession, Suggestion, parse};
use tracing::{debug, info_span, warn};

use crate::io::Source;

/// Hard cap on `parse → fix → re-parse` rounds. Real-world rules converge in
/// 1-3 iterations; anything higher signals a misbehaving rule.
const MAX_ITERATIONS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixLevel {
    /// Only `Applicability::MachineApplicable`.
    Safe,
    /// Also accepts `Applicability::MaybeIncorrect`.
    Suggested,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FixReport {
    /// Number of edits that landed in the buffer.
    pub applied: usize,
    /// Number of fix-loop iterations consumed.
    pub iterations: usize,
    /// `true` when the loop terminated because no edits were left to apply.
    /// `false` means [`MAX_ITERATIONS`] saturated — caller should warn.
    pub converged: bool,
}

pub fn fix_in_place(source: &mut Source, config: &Config, level: FixLevel) -> Result<FixReport> {
    let span = info_span!("fix_in_place", path = %source.display_name());
    let _enter = span.enter();

    let mut report = FixReport::default();

    for _ in 0..MAX_ITERATIONS {
        let outcome = parse(&source.contents);
        let mut session = LintSession::from_config(config);
        let diags = session.run(&outcome.spec);

        let edits = collect_edits(&diags, level);
        if edits.is_empty() {
            report.converged = true;
            break;
        }
        let applied = apply_edits(&mut source.contents, &edits);
        report.applied += applied;
        report.iterations += 1;
        if applied == 0 {
            // Every collected edit was rejected (e.g. all on non-char-
            // boundaries) — no progress is possible.
            report.converged = true;
            break;
        }
    }

    if !report.converged {
        warn!(
            iterations = report.iterations,
            "--fix did not converge after {MAX_ITERATIONS} iterations"
        );
    }
    Ok(report)
}

fn applicable(s: &Suggestion, level: FixLevel) -> bool {
    match (level, s.applicability) {
        (_, Applicability::MachineApplicable) => true,
        (FixLevel::Suggested, Applicability::MaybeIncorrect) => true,
        _ => false,
    }
}

fn collect_edits(diags: &[Diagnostic], level: FixLevel) -> Vec<Edit> {
    let mut edits: Vec<(Applicability, Edit)> = Vec::new();
    for d in diags {
        for s in &d.suggestions {
            if !applicable(s, level) {
                continue;
            }
            for e in &s.edits {
                edits.push((s.applicability, e.clone()));
            }
        }
    }
    // Sort by descending start_byte so applying earlier edits doesn't shift
    // the offsets of later ones. Tie-break by applicability (MachineApplicable
    // first), then by end_byte for deterministic ordering across rule
    // registration order.
    edits.sort_by(|a, b| {
        b.1.span
            .start_byte
            .cmp(&a.1.span.start_byte)
            .then_with(|| applicability_rank(a.0).cmp(&applicability_rank(b.0)))
            .then_with(|| a.1.span.end_byte.cmp(&b.1.span.end_byte))
    });

    let mut accepted: Vec<Edit> = Vec::new();
    let mut last_start = usize::MAX;
    for (_, e) in edits {
        // `>` keeps adjacent edits (`end == last_start`) — they touch but
        // don't overlap.
        if e.span.end_byte > last_start {
            continue;
        }
        last_start = e.span.start_byte;
        accepted.push(e);
    }
    accepted
}

fn applicability_rank(a: Applicability) -> u8 {
    match a {
        Applicability::MachineApplicable => 0,
        Applicability::MaybeIncorrect => 1,
        Applicability::Manual => 2,
        _ => u8::MAX,
    }
}

fn apply_edits(text: &mut String, edits: &[Edit]) -> usize {
    // `edits` is already sorted descending by start_byte.
    let mut applied = 0;
    for e in edits {
        let start = e.span.start_byte;
        let end = e.span.end_byte;
        if end > text.len() || start > end {
            warn!(start, end, len = text.len(), "edit out of bounds, skipping");
            continue;
        }
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            warn!(start, end, "edit straddles UTF-8 codepoint boundary, skipping");
            continue;
        }
        debug!(start, end, replacement_len = e.replacement.len(), "applying edit");
        text.replace_range(start..end, &e.replacement);
        applied += 1;
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec::ast::Span;
    use rpm_spec_analyzer::Suggestion;

    fn span(start: usize, end: usize) -> Span {
        Span::from_bytes(start, end)
    }

    fn edit(start: usize, end: usize, replacement: &str) -> Edit {
        Edit::new(span(start, end), replacement)
    }

    fn diag_with(suggestions: Vec<Suggestion>) -> Diagnostic {
        // Use the missing-changelog metadata as a stand-in — the lint
        // identity is irrelevant for the fixer's logic.
        use rpm_spec_analyzer::Severity;
        let mut d = Diagnostic::new(
            &rpm_spec_analyzer::rules::missing_changelog::METADATA,
            Severity::Warn,
            "test",
            span(0, 0),
        );
        d.suggestions = suggestions;
        d
    }

    fn sugg(applicability: Applicability, edits: Vec<Edit>) -> Suggestion {
        Suggestion::new("msg", applicability, edits)
    }

    #[test]
    fn apply_edits_replaces_in_descending_order() {
        let mut text = "hello world".to_string();
        let edits = vec![
            edit(6, 11, "Rust"), // "world" → "Rust"
            edit(0, 5, "HELLO"), // "hello" → "HELLO"
        ];
        let applied = apply_edits(&mut text, &edits);
        assert_eq!(applied, 2);
        assert_eq!(text, "HELLO Rust");
    }

    #[test]
    fn apply_edits_skips_non_char_boundary() {
        // "Привет" in UTF-8 — every char is 2 bytes.
        let mut text = "Привет".to_string();
        // Span 1..3 splits the first codepoint.
        let edits = vec![edit(1, 3, "?")];
        let applied = apply_edits(&mut text, &edits);
        assert_eq!(applied, 0);
        assert_eq!(text, "Привет");
    }

    #[test]
    fn apply_edits_skips_out_of_bounds() {
        let mut text = "abc".to_string();
        let edits = vec![edit(5, 10, "x")];
        let applied = apply_edits(&mut text, &edits);
        assert_eq!(applied, 0);
        assert_eq!(text, "abc");
    }

    #[test]
    fn collect_edits_drops_overlap_keeps_adjacent() {
        let diags = vec![diag_with(vec![sugg(
            Applicability::MachineApplicable,
            vec![
                edit(0, 5, "A"),  // first in source, adjacent to [5,10)
                edit(5, 10, "B"), // overlaps with [8,12) seen first → dropped
                edit(8, 12, "C"), // latest in source, accepted first
            ],
        )])];
        let collected = collect_edits(&diags, FixLevel::Safe);
        // Desc sort by start_byte processes [8,12) first (accepted),
        // then [5,10) (overlaps [8,12), dropped),
        // then [0,5) (adjacent to [5..] — last_start is 8, 5 not > 8, accepted).
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].span.start_byte, 8);
        assert_eq!(collected[1].span.start_byte, 0);
    }

    #[test]
    fn collect_edits_keeps_adjacent_no_overlap() {
        let diags = vec![diag_with(vec![sugg(
            Applicability::MachineApplicable,
            vec![
                edit(0, 5, "A"),
                edit(5, 10, "B"), // end == 10, next starts at 10 → adjacent, kept
                edit(10, 15, "C"),
            ],
        )])];
        let collected = collect_edits(&diags, FixLevel::Safe);
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].span.start_byte, 10);
        assert_eq!(collected[1].span.start_byte, 5);
        assert_eq!(collected[2].span.start_byte, 0);
    }

    #[test]
    fn collect_edits_filters_by_level() {
        let diags = vec![diag_with(vec![
            sugg(Applicability::MachineApplicable, vec![edit(0, 1, "a")]),
            sugg(Applicability::MaybeIncorrect, vec![edit(2, 3, "b")]),
            sugg(Applicability::Manual, vec![edit(4, 5, "c")]),
        ])];
        let safe = collect_edits(&diags, FixLevel::Safe);
        assert_eq!(safe.len(), 1);
        let suggested = collect_edits(&diags, FixLevel::Suggested);
        assert_eq!(suggested.len(), 2);
    }

    #[test]
    fn collect_edits_tiebreaks_by_applicability() {
        // Two edits at the same start: MachineApplicable should win.
        let diags = vec![diag_with(vec![
            sugg(Applicability::MaybeIncorrect, vec![edit(0, 3, "maybe")]),
            sugg(Applicability::MachineApplicable, vec![edit(0, 3, "safe")]),
        ])];
        let collected = collect_edits(&diags, FixLevel::Suggested);
        // First-accepted is the MachineApplicable one; the other overlaps it.
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].replacement, "safe");
    }
}
