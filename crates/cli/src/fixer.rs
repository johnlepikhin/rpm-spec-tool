//! Apply lint suggestions back to source text.
//!
//! Strategy: gather every edit from every diagnostic for the current pass,
//! sort by descending `start_byte`, drop any edit whose range overlaps an
//! already-accepted one (later, MachineApplicable wins over MaybeIncorrect
//! when both touch the same bytes), apply, and re-parse. Repeat until a pass
//! produces no applicable edits or we hit a sanity cap.

use anyhow::Result;
use rpm_spec_analyzer::{
    Applicability, Diagnostic, Edit, LintSession, Severity, Suggestion, parse,
};
use rpm_spec_analyzer::config::Config;

use crate::io::Source;

/// Maximum number of `parse → fix → re-parse` rounds. A safety bound so a
/// buggy rule that keeps producing the same edit can't loop forever.
const MAX_ITERATIONS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixLevel {
    /// Only `Applicability::MachineApplicable`.
    Safe,
    /// Also accepts `Applicability::MaybeIncorrect`.
    Suggested,
}

#[derive(Debug, Default)]
pub struct FixReport {
    pub applied: usize,
    pub iterations: usize,
}

pub fn fix_in_place(source: &mut Source, config: &Config, level: FixLevel) -> Result<FixReport> {
    let mut report = FixReport::default();

    for _ in 0..MAX_ITERATIONS {
        let outcome = parse(&source.contents);
        let mut session = LintSession::from_config(config);
        let diags = session.run(&outcome.spec);

        let edits = collect_edits(&diags, level);
        if edits.is_empty() {
            break;
        }
        let applied = apply_edits(&mut source.contents, &edits);
        report.applied += applied;
        report.iterations += 1;
        if applied == 0 {
            break;
        }
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
    let mut edits: Vec<(Applicability, Severity, Edit)> = Vec::new();
    for d in diags {
        for s in &d.suggestions {
            if !applicable(s, level) {
                continue;
            }
            for e in &s.edits {
                edits.push((s.applicability, d.severity, e.clone()));
            }
        }
    }
    // Sort by descending start_byte so applying earlier edits doesn't shift
    // the offsets of later ones.
    edits.sort_by(|a, b| {
        b.2.span
            .start_byte
            .cmp(&a.2.span.start_byte)
            // When two edits touch the same start byte, prefer MachineApplicable.
            .then_with(|| applicability_rank(a.0).cmp(&applicability_rank(b.0)))
    });

    let mut accepted: Vec<Edit> = Vec::new();
    let mut last_start = usize::MAX;
    for (_, _, e) in edits {
        if e.span.end_byte > last_start {
            // Overlap with an already-accepted, later-positioned edit.
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
    }
}

fn apply_edits(text: &mut String, edits: &[Edit]) -> usize {
    // `edits` is already sorted descending by start_byte.
    let mut applied = 0;
    for e in edits {
        let start = e.span.start_byte;
        let end = e.span.end_byte;
        if end > text.len() || start > end {
            continue;
        }
        text.replace_range(start..end, &e.replacement);
        applied += 1;
    }
    applied
}
