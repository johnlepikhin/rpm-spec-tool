//! Declarative-macro helpers that generate boilerplate-heavy lints.
//!
//! Both `declare_missing_tag_lint!` and `declare_missing_section_lint!`
//! are `#[macro_export]`ed, which puts them at crate root. They
//! reference [`super::has_top_level_tag`] / [`super::spec_span`]
//! through the `$crate::rules::util::...` facade re-exports, so the
//! reorganisation of `util` into a directory is invisible to call
//! sites.

/// Generate a "missing required tag" lint.
///
/// Phase 1 introduced six near-identical lints (`missing-name-tag`,
/// `missing-version-tag`, ...). They differ only in metadata, the
/// matched [`Tag`] variant, and the message; the visit body, `Lint`
/// impl, and tests are byte-for-byte the same. This macro keeps them
/// declarative and prevents the templated boilerplate from drifting
/// over time.
///
/// Each invocation expands to a `pub mod` with `pub static METADATA`,
/// a `Default + new()` struct, `impl Visit + Lint`, and two unit tests.
#[macro_export]
macro_rules! declare_missing_tag_lint {
    (
        mod $mod_id:ident,
        struct $struct:ident,
        id: $id:literal,
        name: $name:literal,
        description: $desc:literal,
        severity: $sev:expr,
        tag: $tag:pat,
        message: $msg:literal,
        good_fixture: $good:literal,
        bad_fixture: $bad:literal $(,)?
    ) => {
        pub mod $mod_id {
            use rpm_spec::ast::{Span, SpecFile, Tag};

            use $crate::diagnostic::{Diagnostic, LintCategory, Severity};
            use $crate::lint::{Lint, LintMetadata};
            use $crate::rules::util::{has_top_level_tag, spec_span};
            use $crate::visit::Visit;

            pub static METADATA: LintMetadata = LintMetadata {
                id: $id,
                name: $name,
                description: $desc,
                default_severity: $sev,
                category: LintCategory::Packaging,
            };

            #[derive(Debug, Default)]
            pub struct $struct {
                diagnostics: Vec<Diagnostic>,
            }

            impl $struct {
                pub fn new() -> Self {
                    Self::default()
                }
            }

            impl<'ast> Visit<'ast> for $struct {
                fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
                    if !has_top_level_tag(spec, |t: &Tag| matches!(t, $tag)) {
                        self.diagnostics.push(Diagnostic::new(
                            &METADATA,
                            $sev,
                            $msg,
                            spec_span(spec),
                        ));
                    }
                }
            }

            impl Lint for $struct {
                fn metadata(&self) -> &'static LintMetadata {
                    &METADATA
                }
                fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
                    ::std::mem::take(&mut self.diagnostics)
                }
            }

            #[cfg(test)]
            mod tests {
                use super::*;
                use $crate::session::parse;

                fn run(src: &str) -> Vec<Diagnostic> {
                    let outcome = parse(src);
                    let mut lint = $struct::new();
                    lint.visit_spec(&outcome.spec);
                    lint.take_diagnostics()
                }

                #[test]
                fn flags_when_tag_missing() {
                    let diags = run($bad);
                    assert_eq!(diags.len(), 1);
                    assert_eq!(diags[0].lint_id, $id);
                }

                #[test]
                fn silent_when_tag_present() {
                    assert!(run($good).is_empty());
                }
            }
        }
    };
}

/// Generate a "missing required %section" lint.
///
/// Symmetric to [`declare_missing_tag_lint!`] but matches against
/// `Section::BuildScript { kind, .. }`. Each invocation expands to a
/// `pub mod` with metadata, a `Lint` impl, and two unit tests.
#[macro_export]
macro_rules! declare_missing_section_lint {
    (
        mod $mod_id:ident,
        struct $struct:ident,
        id: $id:literal,
        name: $name:literal,
        description: $desc:literal,
        severity: $sev:expr,
        kind: $kind:expr,
        message: $msg:literal,
        good_fixture: $good:literal,
        bad_fixture: $bad:literal $(,)?
    ) => {
        pub mod $mod_id {
            use rpm_spec::ast::{BuildScriptKind, Section, Span, SpecFile, SpecItem};

            use $crate::diagnostic::{Diagnostic, LintCategory, Severity};
            use $crate::lint::{Lint, LintMetadata};
            use $crate::rules::util::spec_span;
            use $crate::visit::Visit;

            pub static METADATA: LintMetadata = LintMetadata {
                id: $id,
                name: $name,
                description: $desc,
                default_severity: $sev,
                category: LintCategory::Packaging,
            };

            #[derive(Debug, Default)]
            pub struct $struct {
                diagnostics: Vec<Diagnostic>,
            }

            impl $struct {
                pub fn new() -> Self {
                    Self::default()
                }
            }

            impl<'ast> Visit<'ast> for $struct {
                fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
                    let target_kind: BuildScriptKind = $kind;
                    let found = spec.items.iter().any(|item| {
                        matches!(
                            item,
                            SpecItem::Section(s)
                                if matches!(
                                    s.as_ref(),
                                    Section::BuildScript { kind, .. } if *kind == target_kind
                                )
                        )
                    });
                    if !found {
                        self.diagnostics.push(Diagnostic::new(
                            &METADATA,
                            $sev,
                            $msg,
                            spec_span(spec),
                        ));
                    }
                }
            }

            impl Lint for $struct {
                fn metadata(&self) -> &'static LintMetadata {
                    &METADATA
                }
                fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
                    ::std::mem::take(&mut self.diagnostics)
                }
            }

            #[cfg(test)]
            mod tests {
                use super::*;
                use $crate::session::parse;

                fn run(src: &str) -> Vec<Diagnostic> {
                    let outcome = parse(src);
                    let mut lint = $struct::new();
                    lint.visit_spec(&outcome.spec);
                    lint.take_diagnostics()
                }

                #[test]
                fn flags_when_section_missing() {
                    let diags = run($bad);
                    assert_eq!(diags.len(), 1);
                    assert_eq!(diags[0].lint_id, $id);
                }

                #[test]
                fn silent_when_section_present() {
                    assert!(run($good).is_empty());
                }
            }
        }
    };
}
