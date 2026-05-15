//! Phase 11 — cross-section consistency for `%package` subpackage
//! declarations.
//!
//! ## Rules
//!
//! - **RPM123 `package-without-description`** — every `%package X` /
//!   `%package -n X` must be paired with a matching `%description`.
//! - **RPM124 `package-without-files`** — every declared subpackage
//!   must own a `%files` section so something actually gets packaged.
//!
//! Both walk the entire spec (including sections nested inside
//! `%if`-blocks), collect declared / described / filed subpackage
//! names normalised against the main package name, and emit on
//! mismatch at the `%package` header span.
//!
//! ## Canonicalisation
//!
//! - `%package foo` (`PackageName::Relative`) ↦ `"<main>-foo"`.
//! - `%package -n foo` (`PackageName::Absolute`) ↦ `"foo"`.
//! - `%description foo` / `%description -n foo` use the same rule
//!   on the `SubpkgRef::{Relative, Absolute}` variants.
//!
//! When either side carries a macro reference inside its name
//! (`%package -n %{name}-devel`), `Text::literal_str()` returns
//! `None` and we conservatively skip the entry — no diagnostic and
//! no claim of mismatch.
//!
//! ## Known limitation (default severity)
//!
//! The parser models `%files` / `%description` blocks **inside** a
//! `%if`-block as `FilesContent::Conditional` (or
//! `PreambleContent::Conditional`) nested inside the surrounding
//! section, not as standalone `Section::Files` / `Section::Description`
//! nodes. Real-world specs (gcc.spec being the obvious example) put
//! arch- or option-conditional subpackage `%files` blocks inside
//! `%if %{build_X}` wrappers — our visitor never sees them as
//! sections, so the check would systematically over-fire.
//!
//! Both rules therefore default to **`Allow`**. Opt in via
//! `--warn package-without-description` / `--warn package-without-files`
//! on projects whose top-level structure keeps `%package` /
//! `%description` / `%files` outside conditional blocks. A future
//! upstream fix (recognising section headers inside conditional
//! bodies) will allow flipping the default back to `Warn` without
//! further changes here.

use std::collections::HashSet;

use rpm_spec::ast::{PackageName, Section, Span, SpecFile, SubpkgRef};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::package_name;
use crate::visit::{self, Visit};

pub static PACKAGE_WITHOUT_DESCRIPTION_METADATA: LintMetadata = LintMetadata {
    id: "RPM123",
    name: "package-without-description",
    description:
        "A `%package` subpackage was declared but no matching `%description` exists; \
         rpmbuild will reject the build.",
    // See module-level docs: parser limitation around `%description`
    // inside `%if`-blocks forces an opt-in default. Promote to
    // `Warn` per-project via `.rpmspec.toml` when applicable.
    default_severity: Severity::Allow,
    category: LintCategory::Correctness,
};

pub static PACKAGE_WITHOUT_FILES_METADATA: LintMetadata = LintMetadata {
    id: "RPM124",
    name: "package-without-files",
    description:
        "A `%package` subpackage was declared but has no matching `%files` section; \
         no payload will be assembled for it.",
    // Same opt-in default as RPM123.
    default_severity: Severity::Allow,
    category: LintCategory::Correctness,
};

// =====================================================================
// Shared collector
// =====================================================================

#[derive(Debug, Default)]
struct Collector {
    main_name: Option<String>,
    /// `(canonical_name, header_span)` for every `%package`
    /// declaration encountered during the visit pass — including
    /// declarations buried inside `%if`-blocks.
    declared: Vec<(String, Span)>,
    /// Canonical names that gained a `%description` somewhere.
    described: HashSet<String>,
    /// Canonical names that gained a `%files` section somewhere.
    filed: HashSet<String>,
}

impl Collector {
    fn record(&mut self, node: &Section<Span>) {
        match node {
            Section::Package { name_arg, data, .. } => {
                if let Some(name) = canonical_package_name(self.main_name.as_deref(), name_arg) {
                    self.declared.push((name, *data));
                }
            }
            Section::Description { subpkg: Some(s), .. } => {
                if let Some(name) = canonical_subpkg_ref(self.main_name.as_deref(), s) {
                    self.described.insert(name);
                }
            }
            Section::Files { subpkg: Some(s), .. } => {
                if let Some(name) = canonical_subpkg_ref(self.main_name.as_deref(), s) {
                    self.filed.insert(name);
                }
            }
            _ => {}
        }
    }
}

fn canonical_package_name(main: Option<&str>, arg: &PackageName) -> Option<String> {
    match arg {
        PackageName::Relative(t) => match (main, t.literal_str()) {
            (Some(m), Some(suffix)) => Some(format!("{m}-{suffix}")),
            _ => None,
        },
        PackageName::Absolute(t) => t.literal_str().map(str::to_owned),
        // `PackageName` is `#[non_exhaustive]`; treat unknown future
        // variants as unidentifiable rather than risk false positives.
        _ => None,
    }
}

fn canonical_subpkg_ref(main: Option<&str>, sr: &SubpkgRef) -> Option<String> {
    match sr {
        SubpkgRef::Relative(t) => match (main, t.literal_str()) {
            (Some(m), Some(suffix)) => Some(format!("{m}-{suffix}")),
            _ => None,
        },
        SubpkgRef::Absolute(t) => t.literal_str().map(str::to_owned),
        _ => None,
    }
}

// =====================================================================
// RPM123 — package-without-description
// =====================================================================

#[derive(Debug, Default)]
pub struct PackageWithoutDescription {
    diagnostics: Vec<Diagnostic>,
    collector: Collector,
}

impl PackageWithoutDescription {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for PackageWithoutDescription {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        self.collector.main_name = package_name(spec).map(str::to_owned);
        visit::walk_spec(self, spec);
    }
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        self.collector.record(node);
        visit::walk_section(self, node);
    }
}

impl Lint for PackageWithoutDescription {
    fn metadata(&self) -> &'static LintMetadata {
        &PACKAGE_WITHOUT_DESCRIPTION_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        let collector = std::mem::take(&mut self.collector);
        for (name, anchor) in &collector.declared {
            if !collector.described.contains(name) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &PACKAGE_WITHOUT_DESCRIPTION_METADATA,
                        Severity::Warn,
                        format!(
                            "subpackage `{name}` is declared via `%package` but has \
                             no matching `%description`"
                        ),
                        *anchor,
                    )
                    .with_suggestion(Suggestion::new(
                        format!("add `%description -n {name}` (or matching relative form)"),
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM124 — package-without-files
// =====================================================================

#[derive(Debug, Default)]
pub struct PackageWithoutFiles {
    diagnostics: Vec<Diagnostic>,
    collector: Collector,
}

impl PackageWithoutFiles {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for PackageWithoutFiles {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        self.collector.main_name = package_name(spec).map(str::to_owned);
        visit::walk_spec(self, spec);
    }
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        self.collector.record(node);
        visit::walk_section(self, node);
    }
}

impl Lint for PackageWithoutFiles {
    fn metadata(&self) -> &'static LintMetadata {
        &PACKAGE_WITHOUT_FILES_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        let collector = std::mem::take(&mut self.collector);
        for (name, anchor) in &collector.declared {
            if !collector.filed.contains(name) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &PACKAGE_WITHOUT_FILES_METADATA,
                        Severity::Warn,
                        format!(
                            "subpackage `{name}` is declared via `%package` but has \
                             no matching `%files` section"
                        ),
                        *anchor,
                    )
                    .with_suggestion(Suggestion::new(
                        format!("add `%files -n {name}` (or matching relative form)"),
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM123 ----

    #[test]
    fn rpm123_flags_relative_package_without_description() {
        let src = "\
Name: hello
Version: 1
Release: 1
Summary: s
License: MIT

%description
main body

%package devel
Summary: devel
";
        let diags = run(src, PackageWithoutDescription::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM123");
        assert!(diags[0].message.contains("hello-devel"));
    }

    #[test]
    fn rpm123_silent_when_paired_relative() {
        let src = "\
Name: hello
Version: 1
Release: 1
Summary: s
License: MIT

%description
main

%package devel
Summary: devel

%description devel
devel body
";
        assert!(run(src, PackageWithoutDescription::new()).is_empty());
    }

    #[test]
    fn rpm123_silent_when_paired_absolute() {
        let src = "\
Name: hello
Version: 1
Release: 1
Summary: s
License: MIT

%description
main

%package -n libhello
Summary: lib

%description -n libhello
lib body
";
        assert!(run(src, PackageWithoutDescription::new()).is_empty());
    }

    #[test]
    fn rpm123_matches_relative_to_absolute_via_main_name() {
        // `%package foo` ⇒ canonical `hello-foo`; `%description -n
        // hello-foo` ⇒ canonical `hello-foo`. Cross-form must still
        // match.
        let src = "\
Name: hello
Version: 1
Release: 1
Summary: s
License: MIT

%description
main

%package foo
Summary: foo

%description -n hello-foo
foo body
";
        assert!(run(src, PackageWithoutDescription::new()).is_empty());
    }

    #[test]
    fn rpm123_silent_when_name_is_macro() {
        // Macro reference in subpackage name → conservative skip; no
        // false claim of mismatch.
        let src = "\
Name: hello
Version: 1
Release: 1
Summary: s
License: MIT

%description
main

%package -n %{name}-libs
Summary: libs
";
        assert!(run(src, PackageWithoutDescription::new()).is_empty());
    }

    #[test]
    fn rpm123_flags_subpackage_declared_inside_conditional() {
        // The visitor must descend into conditionals. Declared inside
        // %if, no matching description anywhere → fires.
        let src = "\
Name: hello
Version: 1
Release: 1
Summary: s
License: MIT

%description
main

%if 1
%package devel
Summary: devel
%endif
";
        let diags = run(src, PackageWithoutDescription::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    // ---- RPM124 ----

    #[test]
    fn rpm124_flags_relative_package_without_files() {
        let src = "\
Name: hello
Version: 1
Release: 1
Summary: s
License: MIT

%description
main

%package devel
Summary: devel

%description devel
devel body

%files
/usr/bin/hello
";
        // %files refers to main; no %files devel → RPM124 fires.
        let diags = run(src, PackageWithoutFiles::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM124");
        assert!(diags[0].message.contains("hello-devel"));
    }

    #[test]
    fn rpm124_silent_when_paired() {
        let src = "\
Name: hello
Version: 1
Release: 1
Summary: s
License: MIT

%description
main

%package devel
Summary: devel

%description devel
body

%files
/usr/bin/hello

%files devel
/usr/include/hello.h
";
        assert!(run(src, PackageWithoutFiles::new()).is_empty());
    }
}
