//! RPM512 `redundant-default-defattr` — flag `%defattr(-,root,root,-)`
//! lines. Modern RPM defaults to these exact attributes, so the
//! directive is dead boilerplate.
//!
//! Older RPM (~ 4.4 and earlier) required `%defattr` to be present in
//! every `%files` block. Modern packaging guidelines drop the line.

use rpm_spec::ast::{AttrField, FileDirective, FilesContent, Section, Span, SpecFile, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM512",
    name: "redundant-default-defattr",
    description: "`%defattr(-,root,root,-)` is the default for modern RPM — drop the line.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%defattr(-,root,root,-)` is the default for modern RPM — drop the line.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RedundantDefaultDefattr {
    diagnostics: Vec<Diagnostic>,
}

impl RedundantDefaultDefattr {
    pub fn new() -> Self {
        Self::default()
    }

    fn scan_content(&mut self, content: &[FilesContent<Span>]) {
        for it in content {
            match it {
                FilesContent::Entry(e) => {
                    for d in &e.directives {
                        if let FileDirective::Defattr(fields) = d
                            && is_default_root(fields)
                        {
                            self.diagnostics.push(
                                Diagnostic::new(
                                    &METADATA,
                                    Severity::Warn,
                                    "`%defattr(-,root,root,-)` is the default for modern RPM — \
                                     drop the line",
                                    e.data,
                                )
                                .with_suggestion(Suggestion::new(
                                    "remove the redundant `%defattr` line",
                                    Vec::new(),
                                    Applicability::Manual,
                                )),
                            );
                        }
                    }
                }
                FilesContent::Conditional(c) => {
                    for branch in &c.branches {
                        self.scan_content(&branch.body);
                    }
                    if let Some(els) = &c.otherwise {
                        self.scan_content(els);
                    }
                }
                _ => {}
            }
        }
    }
}

fn is_default_root(fields: &rpm_spec::ast::DefattrFields) -> bool {
    let user_root = matches!(&fields.user, AttrField::Name(t) if t.literal_str().is_some_and(|s| s.trim() == "root"));
    let group_root = matches!(&fields.group, AttrField::Name(t) if t.literal_str().is_some_and(|s| s.trim() == "root"));
    let fmode_default = matches!(fields.fmode, AttrField::Default);
    let dmode_default = matches!(fields.dmode.as_ref(), None | Some(AttrField::Default));
    user_root && group_root && fmode_default && dmode_default
}

impl<'ast> Visit<'ast> for RedundantDefaultDefattr {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Files { content, .. } = boxed.as_ref() else {
                continue;
            };
            self.scan_content(content);
        }
    }
}

impl Lint for RedundantDefaultDefattr {
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
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<RedundantDefaultDefattr>(src)
    }

    #[test]
    fn flags_default_defattr_with_dash_dmode() {
        let src = "Name: x\n%files\n%defattr(-,root,root,-)\n%{_bindir}/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM512");
    }

    #[test]
    fn flags_default_defattr_without_dmode() {
        let src = "Name: x\n%files\n%defattr(-,root,root)\n%{_bindir}/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_custom_user() {
        let src = "Name: x\n%files\n%defattr(-,daemon,daemon,-)\n%{_bindir}/foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_custom_mode() {
        let src = "Name: x\n%files\n%defattr(0644,root,root,0755)\n%{_bindir}/foo\n";
        assert!(run(src).is_empty());
    }
}
