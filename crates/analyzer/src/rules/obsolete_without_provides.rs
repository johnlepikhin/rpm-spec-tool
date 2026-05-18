//! RPM034 `obsolete-without-provides` — when a package obsoletes
//! another, it should also `Provides:` the obsoleted name so that
//! upgrading users keep getting the functionality. Without a matching
//! Provides, dependent packages break on upgrade.
//!
//! Subpackage-aware: each `%package` block is checked against its own
//! Obsoletes and Provides sets. Skips:
//! - obsoletes whose name contains a macro (can't compare literally),
//! - obsoletes that look like file paths (`Obsoletes: /usr/bin/...`),
//! - obsoletes carrying a version constraint (`< X.Y` etc.) — that's
//!   the "we are deleting these old versions" idiom, not a rename.
//!
//! The version-range escape hatch removes the bulk of false positives
//! from real distros, where `Obsoletes: <old> < X.Y` is the canonical
//! "we are dropping these old versions" pattern.

use std::collections::HashSet;

use rpm_spec::ast::{DepExpr, Span, SpecFile, Tag, TagValue};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::iter_packages;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM034",
    name: "obsolete-without-provides",
    description: "Each unconstrained Obsoletes entry should be matched by a Provides of the same name to keep upgrades smooth.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Each unconstrained Obsoletes entry should be matched by a Provides of the same name to keep upgrades smooth.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ObsoleteWithoutProvides {
    diagnostics: Vec<Diagnostic>,
}

impl ObsoleteWithoutProvides {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ObsoleteWithoutProvides {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            // Pre-collect provides names once per package scope.
            let mut provides_names: HashSet<&str> = HashSet::new();
            for item in pkg.items() {
                if !matches!(item.tag, Tag::Provides) {
                    continue;
                }
                if let TagValue::Dep(expr) = &item.value {
                    walk_collect_names(expr, &mut provides_names);
                }
            }

            // Walk Obsoletes items keeping per-item span so the
            // diagnostic points at the offending line.
            for item in pkg.items() {
                if !matches!(item.tag, Tag::Obsoletes) {
                    continue;
                }
                let TagValue::Dep(expr) = &item.value else {
                    continue;
                };
                for atom in collect_atoms(expr) {
                    if atom.constraint.is_some() {
                        // `Obsoletes: foo < 1.2` — explicitly bounded,
                        // means "delete these old versions", not a
                        // rename. Distros routinely keep this without
                        // a matching Provides.
                        continue;
                    }
                    let Some(obs_name) = atom.name.literal_str() else {
                        continue; // macroized name, can't compare
                    };
                    if obs_name.starts_with('/') {
                        continue; // file-path obsoletes are uncommon and legitimate
                    }
                    if !provides_names.contains(obs_name) {
                        self.diagnostics.push(Diagnostic::new(
                            &METADATA,
                            Severity::Warn,
                            format!(
                                "`Obsoletes: {obs_name}` has no matching `Provides:` and no \
                                 version range — upgraders of {obs_name} may lose the \
                                 functionality. Add `Provides: {obs_name}` if this is a \
                                 rename, or `Obsoletes: {obs_name} < X.Y` if it is a removal."
                            ),
                            item.data,
                        ));
                    }
                }
            }
        }
    }
}

fn collect_atoms(expr: &DepExpr) -> Vec<&rpm_spec::ast::DepAtom> {
    let mut out = Vec::new();
    walk_atoms(expr, &mut out);
    out
}

fn walk_atoms<'a>(expr: &'a DepExpr, out: &mut Vec<&'a rpm_spec::ast::DepAtom>) {
    use rpm_spec::ast::BoolDep;
    // Both `DepExpr` and `BoolDep` are `#[non_exhaustive]` and live in
    // another crate (`rpm-spec`), so the compiler forces a trailing
    // wildcard. We still name every variant we know about explicitly so
    // that, when the project is updated to a newer rpm-spec, a reviewer
    // running `cargo expand` / `git blame` sees exactly which variants
    // were considered. Future variants land in the `_` arm — `cargo
    // semver-checks` / `cargo update` review must re-audit this match.
    // Same discipline as `bcond_on_non_fedora.rs:107-127`.
    match expr {
        DepExpr::Atom(a) => out.push(a),
        DepExpr::Rich(b) => match b.as_ref() {
            BoolDep::And(xs) | BoolDep::Or(xs) | BoolDep::With(xs) => {
                for x in xs {
                    walk_atoms(x, out);
                }
            }
            BoolDep::If {
                cond,
                then,
                otherwise,
            }
            | BoolDep::Unless {
                cond,
                then,
                otherwise,
            } => {
                walk_atoms(cond, out);
                walk_atoms(then, out);
                if let Some(o) = otherwise {
                    walk_atoms(o, out);
                }
            }
            BoolDep::Without { left, right } => {
                walk_atoms(left, out);
                walk_atoms(right, out);
            }
            // Forced by `#[non_exhaustive]` on a foreign-crate enum;
            // see comment above. Re-audit on every `rpm-spec` bump.
            // NOTE: kept as `#[allow]` (not `#[expect]`) because the wildcard
            // arm is reachable today — `#[non_exhaustive]` makes it valid for
            // current rustc, so `#[expect(unreachable_patterns)]` would emit
            // `unfulfilled_lint_expectations`.
            #[allow(unreachable_patterns)]
            _ => {}
        },
        // Forced by `#[non_exhaustive]` on a foreign-crate enum;
        // see comment above. Re-audit on every `rpm-spec` bump.
        // NOTE: kept as `#[allow]` (not `#[expect]`) because the wildcard
        // arm is reachable today — see explanation on the inner arm above.
        #[allow(unreachable_patterns)]
        _ => {}
    }
}

fn walk_collect_names<'a>(expr: &'a DepExpr, out: &mut HashSet<&'a str>) {
    for a in collect_atoms(expr) {
        if let Some(name) = a.name.literal_str() {
            out.insert(name);
        }
    }
}

impl Lint for ObsoleteWithoutProvides {
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
        run_lint::<ObsoleteWithoutProvides>(src)
    }

    #[test]
    fn flags_obsolete_without_provides() {
        let diags = run("Name: hello\nObsoletes: old-hello\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM034");
    }

    #[test]
    fn silent_when_provides_matches() {
        assert!(run("Name: hello\nObsoletes: old-hello\nProvides: old-hello\n").is_empty());
    }

    #[test]
    fn skips_path_obsoletes() {
        // file-path obsoletes are rare and legitimate; we conservatively
        // don't flag them
        assert!(run("Name: hello\nObsoletes: /usr/bin/old\n").is_empty());
    }

    #[test]
    fn skips_version_constrained_obsoletes() {
        // `Obsoletes: foo < 1.2` is the canonical "drop these old
        // versions" idiom — no Provides needed, no diagnostic expected.
        assert!(run("Name: hello\nObsoletes: old-hello < 1.2\n").is_empty());
        assert!(run("Name: hello\nObsoletes: old-hello <= 1.2\n").is_empty());
    }

    #[test]
    fn skips_macroized_obsoletes() {
        // Name with a macro can't be matched literally; conservatively skip.
        assert!(run("Name: hello\nObsoletes: %{macro_name}\n").is_empty());
    }

    #[test]
    fn flags_subpackage_obsolete_without_provides() {
        // Regression lock for the subpackage-aware code path: the
        // subpackage `foo` declares `Obsoletes: old-foo` with no
        // matching `Provides:` *inside the same subpackage* (provides
        // in main package don't count).
        let src = "Name: main\n\
%package -n foo\n\
Summary: standalone\n\
Obsoletes: old-foo\n\
%description -n foo\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert!(diags[0].message.contains("old-foo"));
    }
}
