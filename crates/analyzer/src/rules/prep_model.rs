//! Shared model of the `%prep` section.
//!
//! Locates the prep body and provides shared helpers for inspecting
//! `%setup` / `%autosetup` / `%patch` / `%autopatch` invocations.
//! Existing rules (RPM063/RPM064/RPM306) historically scanned the body
//! themselves; new prep-related rules (RPM470/RPM471/RPM472/RPM474/
//! RPM475/RPM476) share this module so flag-parsing logic lives in one
//! place.

use rpm_spec::ast::{BuildScriptKind, Section, ShellBody, Span, SpecFile, SpecItem};

/// Locate the first top-level `%prep` section's body. Returns `None`
/// when the spec has no `%prep` — `missing-prep-section` (RPM016) covers
/// that case separately.
pub(crate) fn find_prep_body(spec: &SpecFile<Span>) -> Option<&ShellBody<Span>> {
    find_prep_section(spec).map(|(body, _)| body)
}

/// Same as [`find_prep_body`] but also exposes the section span,
/// useful as a diagnostic anchor when the body itself is empty.
pub(crate) fn find_prep_body_with_span(spec: &SpecFile<Span>) -> Option<(&ShellBody<Span>, Span)> {
    find_prep_section(spec)
}

fn find_prep_section(spec: &SpecFile<Span>) -> Option<(&ShellBody<Span>, Span)> {
    for item in &spec.items {
        if let SpecItem::Section(boxed) = item
            && let Section::BuildScript {
                kind: BuildScriptKind::Prep,
                body,
                data,
            } = boxed.as_ref()
        {
            return Some((body, *data));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    #[test]
    fn find_prep_body_returns_some_when_prep_present() {
        let outcome = parse("Name: x\n%prep\n%setup -q\n");
        let body = find_prep_body(&outcome.spec).expect("prep present");
        assert!(!body.lines.is_empty());
    }

    #[test]
    fn find_prep_body_returns_none_without_prep() {
        let outcome = parse("Name: x\n");
        assert!(find_prep_body(&outcome.spec).is_none());
    }

    #[test]
    fn find_prep_body_with_span_exposes_section_span() {
        let outcome = parse("Name: x\n%prep\n%setup -q\n");
        let (_body, span) = find_prep_body_with_span(&outcome.spec).expect("prep present");
        assert!(span.end_byte > span.start_byte);
    }
}
