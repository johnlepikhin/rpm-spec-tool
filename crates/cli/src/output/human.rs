//! Human-readable diagnostic rendering via `codespan-reporting`.

use anyhow::Result;
use codespan_reporting::diagnostic::{Diagnostic as CsDiag, Label, Severity as CsSeverity};
use codespan_reporting::files::SimpleFile;
use codespan_reporting::term::termcolor::{ColorChoice as TermColor, StandardStream};
use codespan_reporting::term::{self, Config};
use rpm_spec_analyzer::{Diagnostic, Severity};
use std::io::IsTerminal;

use crate::app::ColorChoice;
use crate::io::Source;

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
    Ok(())
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
