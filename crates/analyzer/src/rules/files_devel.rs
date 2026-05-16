//! RPM364 `devel-file-in-non-devel-package` — development artifacts
//! (`.h` headers, `.pc` pkgconfig files, CMake config files,
//! unversioned `.so` symlinks) belong in a `-devel` subpackage.
//!
//! Mixing them into the runtime package forces every installation to
//! drag the development ecosystem along (pkgconfig, headers, cmake
//! metadata) which inflates dependencies and breaks distributions that
//! cleanly split runtime from development.
//!
//! Detection:
//!
//! 1. Get the canonical name of the `%files` section's package via
//!    `for_each_files_entry_with_subpkg`.
//! 2. If the package name ends in `-devel` (or `-headers` / `-static`),
//!    the file is in the right place — skip.
//! 3. Otherwise, if the entry is a devel artifact per
//!    [`KindHints`] — flag.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry_with_subpkg, resolve_subpkg_name};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::package_name;
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM364",
    name: "devel-file-in-non-devel-package",
    description: "A development artifact (`.h`, `.pc`, CMake config, unversioned `.so`) is \
                  shipped in a non-`-devel` package. Move it to a `-devel` subpackage so \
                  runtime installs do not drag in the development ecosystem.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct DevelFileInNonDevelPackage {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl DevelFileInNonDevelPackage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DevelFileInNonDevelPackage {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        let main = package_name(spec).map(str::to_owned);

        for_each_files_entry_with_subpkg(spec, |subpkg, entry| {
            let pkg = resolve_subpkg_name(main.as_deref(), subpkg)
                .or_else(|| main.clone())
                .unwrap_or_default();
            if is_devel_like_package(&pkg) {
                return;
            }
            let cls = classifier.classify(entry);
            let Some(reason) = devel_artifact_reason(&cls.kind_hints) else {
                return;
            };
            let path = cls.resolved_path.as_deref().unwrap_or("");
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`{path}` is a {reason}; in package `{pkg}` instead of a `-devel` subpackage"
                ),
                cls.span(),
            ));
        });
    }
}

impl Lint for DevelFileInNonDevelPackage {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
    }
}

fn is_devel_like_package(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with("-devel")
        || lower.ends_with("-headers")
        || lower.ends_with("-static")
        || lower.ends_with("-dev")
}

fn devel_artifact_reason(h: &crate::files::KindHints) -> Option<&'static str> {
    if h.is_devel_header {
        Some("development header (`.h`/`.hpp`)")
    } else if h.is_pkgconfig {
        Some("pkgconfig file (`.pc`)")
    } else if h.is_cmake_config {
        Some("CMake config file")
    } else if h.is_unversioned_so {
        Some("unversioned `.so` symlink (development artifact)")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;
    use rpm_spec_profile::{MacroEntry, Profile, Provenance};

    fn fedora_profile() -> Profile {
        let mut p = Profile::default();
        for (name, body) in [
            ("_prefix", "/usr"),
            ("_libdir", "/usr/lib64"),
            ("_includedir", "/usr/include"),
        ] {
            p.macros
                .insert(name, MacroEntry::literal(body, Provenance::Override));
        }
        p
    }

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = DevelFileInNonDevelPackage::new();
        lint.set_profile(&fedora_profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_header_in_main_package() {
        let src = "Name: foo\n%files\n/usr/include/foo.h\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM364");
        assert!(diags[0].message.contains("foo.h"));
    }

    #[test]
    fn flags_pkgconfig_in_main_package() {
        let src = "Name: foo\n%files\n/usr/lib64/pkgconfig/foo.pc\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_unversioned_so_in_main() {
        let src = "Name: foo\n%files\n/usr/lib64/libfoo.so\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_header_in_devel_subpackage() {
        let src = "Name: foo\n\
%package devel\n\
Summary: dev\n\
%description devel\nbody\n\
%files devel\n\
/usr/include/foo.h\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_versioned_so_in_main() {
        // Runtime library is correctly in main package.
        let src = "Name: foo\n%files\n/usr/lib64/libfoo.so.1\n/usr/lib64/libfoo.so.1.2.3\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_header_in_dev_named_subpackage() {
        let src = "Name: foo\n\
%package dev\n\
Summary: dev\n\
%description dev\nbody\n\
%files dev\n\
/usr/include/foo.h\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_cmake_config_in_main() {
        let src = "Name: foo\n%files\n/usr/lib64/cmake/Foo/FooConfig.cmake\n";
        assert_eq!(run(src).len(), 1);
    }
}
