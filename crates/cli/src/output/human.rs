//! Human-readable diagnostic rendering via `codespan-reporting`.

use std::collections::HashMap;
use std::io::IsTerminal;

use anyhow::Result;
use codespan_reporting::diagnostic::{Diagnostic as CsDiag, Label, Severity as CsSeverity};
use codespan_reporting::files::SimpleFile;
use codespan_reporting::term::termcolor::{
    Color, ColorChoice as TermColor, ColorSpec, StandardStream, WriteColor,
};
use codespan_reporting::term::{self, Config};
use rpm_spec_analyzer::{Diagnostic, Severity};

use crate::app::ColorChoice;
use crate::io::Source;

/// Cap on per-lint rows in the summary footer. Real-world specs have
/// long-tail lint distributions; showing 10 keeps the footer tight
/// while preserving signal.
const MAX_SUMMARY_ROWS: usize = 10;

pub fn render(items: &[(Source, Vec<Diagnostic>)], color: ColorChoice) -> Result<()> {
    let stream = StandardStream::stderr(resolve_color(color));
    let mut writer = stream.lock();
    let cfg = Config::default();

    for (source, diags) in items {
        if diags.is_empty() {
            continue;
        }
        let file = SimpleFile::new(source.display_name(), source.contents.as_str());
        for diag in diags {
            let cs = to_cs_diag(diag);
            term::emit(&mut writer, &cfg, &file, &cs)?;
        }
    }
    render_summary(items, &mut writer)?;
    Ok(())
}

/// Aggregated counts for the summary footer.
struct Stats {
    errors: usize,
    warnings: usize,
    files_with_diags: usize,
    /// `lint_id → (count, lint_name, worst_severity_seen)`.
    by_lint: HashMap<&'static str, (usize, &'static str, Severity)>,
}

fn aggregate(items: &[(Source, Vec<Diagnostic>)]) -> Stats {
    let mut s = Stats {
        errors: 0,
        warnings: 0,
        files_with_diags: 0,
        by_lint: HashMap::new(),
    };
    for (_, diags) in items {
        if !diags.is_empty() {
            s.files_with_diags += 1;
        }
        for d in diags {
            match d.severity {
                Severity::Deny => s.errors += 1,
                Severity::Warn => s.warnings += 1,
                // `Allow` is filtered out at session-load; if one
                // appears here it's a transient note — don't count.
                Severity::Allow => {}
            }
            let entry =
                s.by_lint
                    .entry(d.lint_id)
                    .or_insert((0, d.lint_name, d.severity));
            entry.0 += 1;
            // Promote stored severity to the worst seen so the row
            // colour reflects the highest-impact occurrence.
            if matches!(d.severity, Severity::Deny) {
                entry.2 = Severity::Deny;
            }
        }
    }
    s
}

fn render_summary<W: WriteColor>(items: &[(Source, Vec<Diagnostic>)], w: &mut W) -> Result<()> {
    let stats = aggregate(items);
    if stats.errors == 0 && stats.warnings == 0 {
        return Ok(());
    }

    writeln!(w)?;

    // Header line: "summary: N warnings[, M errors][ across K files]".
    let header_color = if stats.errors > 0 {
        Color::Red
    } else {
        Color::Yellow
    };
    w.set_color(ColorSpec::new().set_fg(Some(header_color)).set_bold(true))?;
    write!(w, "summary")?;
    w.reset()?;
    write!(w, ": ")?;

    let mut parts: Vec<String> = Vec::new();
    if stats.errors > 0 {
        parts.push(format!("{} {}", stats.errors, plural(stats.errors, "error")));
    }
    if stats.warnings > 0 {
        parts.push(format!(
            "{} {}",
            stats.warnings,
            plural(stats.warnings, "warning")
        ));
    }
    write!(w, "{}", parts.join(", "))?;
    if stats.files_with_diags > 1 {
        write!(
            w,
            " across {} {}",
            stats.files_with_diags,
            plural(stats.files_with_diags, "file")
        )?;
    }
    writeln!(w)?;

    // Per-lint breakdown, descending by count, alphabetical id as
    // tiebreaker. Cap at MAX_SUMMARY_ROWS to keep the footer tight.
    let mut rows: Vec<(&'static str, usize, &'static str, Severity)> = stats
        .by_lint
        .into_iter()
        .map(|(id, (count, name, sev))| (id, count, name, sev))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    let total_lints = rows.len();
    let max_count = rows.first().map(|r| r.1).unwrap_or(0);
    let count_width = max_count.to_string().len().max(1);
    let shown = rows.iter().take(MAX_SUMMARY_ROWS);
    for (id, count, name, sev) in shown {
        write!(w, "  ")?;
        let row_color = match sev {
            Severity::Deny => Color::Red,
            _ => Color::Yellow,
        };
        w.set_color(ColorSpec::new().set_fg(Some(row_color)).set_bold(true))?;
        write!(w, "{count:>count_width$}")?;
        w.reset()?;
        write!(w, " × ")?;
        w.set_color(ColorSpec::new().set_fg(Some(row_color)))?;
        write!(w, "{id}")?;
        w.reset()?;
        writeln!(w, " [{name}]")?;
    }
    if total_lints > MAX_SUMMARY_ROWS {
        let hidden = total_lints - MAX_SUMMARY_ROWS;
        writeln!(w, "  … and {hidden} more {}", plural(hidden, "lint"))?;
    }

    Ok(())
}

fn plural(n: usize, base: &str) -> String {
    if n == 1 {
        base.to_string()
    } else {
        format!("{base}s")
    }
}

fn resolve_color(choice: ColorChoice) -> TermColor {
    match choice {
        ColorChoice::Always => TermColor::Always,
        ColorChoice::Never => TermColor::Never,
        ColorChoice::Auto => {
            if std::io::stderr().is_terminal() {
                TermColor::Auto
            } else {
                TermColor::Never
            }
        }
    }
}

fn to_cs_diag(d: &Diagnostic) -> CsDiag<()> {
    let sev = match d.severity {
        Severity::Deny => CsSeverity::Error,
        Severity::Warn => CsSeverity::Warning,
        // `Allow` should never appear on an emitted diagnostic.
        Severity::Allow => CsSeverity::Note,
    };
    let header = format!("[{}] {}", d.lint_name, d.message);
    let primary = Label::primary((), span_to_range(&d.primary_span));
    let mut labels = vec![primary];
    for l in &d.labels {
        labels.push(Label::secondary((), span_to_range(&l.span)).with_message(&l.message));
    }

    let mut notes = Vec::new();
    for s in &d.suggestions {
        notes.push(format!("help: {} ({:?})", s.message, s.applicability));
    }

    CsDiag::new(sev)
        .with_message(header)
        .with_code(d.lint_id)
        .with_labels(labels)
        .with_notes(notes)
}

fn span_to_range(s: &rpm_spec::ast::Span) -> std::ops::Range<usize> {
    s.start_byte..s.end_byte
}
