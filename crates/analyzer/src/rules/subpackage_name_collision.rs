//! RPM301 `subpackage-name-collision` — flag two `%package` blocks that
//! resolve to the same canonical package name.
//!
//! Typical collisions caught:
//!
//! ```rpm
//! %package devel                       # → "<main>-devel"
//! %package -n %{name}-devel            # → "<main>-devel" — collision
//! ```
//!
//! ```rpm
//! Name: hello
//! %package -n hello                    # collides with the main package
//! ```
//!
//! RPM keeps the *last* `%package` block; earlier ones lose their
//! `%files` / `%description` and become dead code. The diagnostic points
//! at the colliding header with a label on the first occurrence.
//!
//! Macro handling is conservative: only the `%name` / `%{name}`
//! reference is substituted (against the main package's literal `Name:`
//! tag). Names that still contain unresolved macros after that are
//! skipped — a false-negative is preferred to a false-positive built on
//! guesses about runtime values.

use std::collections::HashMap;

use rpm_spec::ast::{
    PackageName, Section, Span, SpecFile, SpecItem, Tag, TagValue, Text, TextSegment,
};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::package_name;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM301",
    name: "subpackage-name-collision",
    description: "Two `%package` blocks (or `%package` and the main package) resolve to the same \
                  canonical name; RPM keeps the last block and the earlier one's `%files` / \
                  `%description` becomes dead code.",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct SubpackageNameCollision {
    diagnostics: Vec<Diagnostic>,
}

impl SubpackageNameCollision {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SubpackageNameCollision {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let main_name = package_name(spec).map(str::to_owned);

        // Index from canonical name → first span seen.
        let mut seen: HashMap<String, Span> = HashMap::new();

        // Main package occupies the canonical name first (if it has one).
        if let Some(ref n) = main_name {
            seen.insert(n.clone(), main_package_header_span(spec));
        }

        for item in &spec.items {
            collect_packages(item, main_name.as_deref(), &mut seen, &mut self.diagnostics);
        }
    }
}

fn collect_packages(
    item: &SpecItem<Span>,
    main_name: Option<&str>,
    seen: &mut HashMap<String, Span>,
    out: &mut Vec<Diagnostic>,
) {
    match item {
        SpecItem::Section(boxed) => {
            if let Section::Package {
                name_arg,
                data: header,
                ..
            } = boxed.as_ref()
                && let Some(canonical) = resolve_package_name(main_name, name_arg)
            {
                match seen.get(&canonical) {
                    None => {
                        seen.insert(canonical, *header);
                    }
                    Some(&first) => {
                        out.push(
                            Diagnostic::new(
                                &METADATA,
                                Severity::Deny,
                                format!(
                                    "`%package` block resolves to `{canonical}`, which already exists",
                                ),
                                *header,
                            )
                            .with_label(first, "previously declared here"),
                        );
                    }
                }
            }
        }
        SpecItem::Conditional(c) => {
            // Subpackages declared inside `%if`/`%else` are alternatives.
            // We still detect collisions *within* one branch by giving
            // each branch its own `seen` map — but we do *not* inherit
            // the main package's name into branches, because RPM treats
            // the spec as already-resolved by then. Mirroring
            // `iter_packages`' "intentionally skip conditional %package"
            // would lose real bugs like `%package devel` twice inside
            // one branch.
            for branch in &c.branches {
                let mut branch_seen: HashMap<String, Span> = HashMap::new();
                for nested in &branch.body {
                    collect_packages(nested, main_name, &mut branch_seen, out);
                }
            }
            if let Some(els) = &c.otherwise {
                let mut branch_seen: HashMap<String, Span> = HashMap::new();
                for nested in els {
                    collect_packages(nested, main_name, &mut branch_seen, out);
                }
            }
        }
        _ => {}
    }
}

fn main_package_header_span(spec: &SpecFile<Span>) -> Span {
    // Best-effort: the main `Name:` line is the closest stable anchor
    // for a "main package declared here" label. Falling back to the
    // spec span (`SpecFile.data`) keeps us correct when `Name:` is
    // absent — the missing-name-tag rule will already complain.
    for item in &spec.items {
        if let SpecItem::Preamble(p) = item
            && matches!(p.tag, Tag::Name)
            && matches!(p.value, TagValue::Text(_))
        {
            return p.data;
        }
    }
    spec.data
}

fn resolve_package_name(main_name: Option<&str>, arg: &PackageName) -> Option<String> {
    match arg {
        PackageName::Relative(t) => {
            let suffix = expand_name_macro(t, main_name)?;
            // `%package devel` → "<main>-devel". Without main_name we
            // can't form the canonical name, so skip.
            let main = main_name?;
            Some(format!("{main}-{suffix}"))
        }
        PackageName::Absolute(t) => expand_name_macro(t, main_name),
        _ => None,
    }
}

/// Try to resolve `text` to a plain string, substituting only the
/// `%name` / `%{name}` references against `main_name`. Returns `None`
/// when any other macro remains — we can't safely compare expressions
/// whose values are decided at build time.
fn expand_name_macro(text: &Text, main_name: Option<&str>) -> Option<String> {
    let mut out = String::new();
    for seg in &text.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(m) => {
                if m.name == "name" {
                    let n = main_name?;
                    out.push_str(n);
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

impl Lint for SubpackageNameCollision {
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
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SubpackageNameCollision::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_relative_vs_absolute_name_collision() {
        // `%package devel` (Relative) and `%package -n hello-devel`
        // (Absolute, literal) both resolve to "hello-devel".
        let src = "Name: hello\n\
%package devel\n\
Summary: dev\n\
%description devel\nd\n\
%package -n hello-devel\n\
Summary: dev2\n\
%description -n hello-devel\nd2\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM301");
        assert!(diags[0].message.contains("hello-devel"));
    }

    #[test]
    fn flags_two_relative_subpackages_with_same_suffix() {
        let src = "Name: hello\n\
%package devel\n\
Summary: a\n\
%description devel\nx\n\
%package devel\n\
Summary: b\n\
%description devel\ny\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("hello-devel"));
    }

    #[test]
    fn flags_subpackage_colliding_with_main() {
        // `%package -n hello` collides with the main package itself.
        let src = "Name: hello\n\
%package -n hello\n\
Summary: shadow\n\
%description -n hello\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_distinct_subpackages() {
        let src = "Name: hello\n\
%package devel\n\
Summary: a\n\
%description devel\nx\n\
%package libs\n\
Summary: b\n\
%description libs\ny\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_subpackage_name_unresolvable() {
        // Parser currently drops `%package -n` with macro-valued names,
        // so this case is exercised via the resolver helper directly:
        // a `PackageName::Absolute` containing an unknown macro must
        // produce `None`, ensuring future parser support won't falsely
        // collide such names with anything.
        let unknown_macro = rpm_spec::ast::Text {
            segments: vec![rpm_spec::ast::TextSegment::macro_ref(
                rpm_spec::ast::MacroRef {
                    kind: rpm_spec::ast::MacroKind::Braced,
                    name: "flavor".into(),
                    args: Vec::new(),
                    conditional: rpm_spec::ast::ConditionalMacro::None,
                    with_value: None,
                },
            )],
        };
        let arg = rpm_spec::ast::PackageName::Absolute(unknown_macro);
        assert_eq!(resolve_package_name(Some("hello"), &arg), None);
    }

    #[test]
    fn silent_when_main_name_missing() {
        // No `Name:` ⇒ can't resolve relative `%package devel`. Stay
        // quiet; missing-name-tag (RPM010) handles the root issue.
        let src = "%package devel\n\
Summary: a\n\
%description devel\nx\n\
%package devel\n\
Summary: b\n\
%description devel\ny\n";
        // Without a main name we can't form "<main>-devel" canonically,
        // so the collision check sees two `None` entries and stays silent.
        assert!(run(src).is_empty());
    }
}
