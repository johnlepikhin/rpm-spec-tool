//! RPM312 `spec-filename-mismatch` — the on-disk file name should be
//! `<Name>.spec`.
//!
//! Most build systems (`fedpkg`, `rpmbuild -ba`, `osc build`, Mock,
//! Koji) look up the package's `.spec` file by the package name plus
//! the `.spec` suffix. A mismatch between `Name:` and the file name
//! either breaks discovery outright or causes subtle pairing bugs
//! when one source directory hosts multiple specs.
//!
//! The check stays silent when:
//!
//! - The source is stdin / an in-memory string (no file name to
//!   compare against).
//! - `Name:` is missing or contains macros — comparison would be a
//!   guess.
//! - The file name is not valid UTF-8 (rare; would also be unusable as
//!   a package name).
//!
//! ## Case sensitivity
//!
//! The comparison is **byte-exact**, matching RPM's own filename
//! handling on Linux (the canonical target). On case-insensitive file
//! systems (macOS HFS+/APFS, Windows NTFS by default) a file called
//! `Hello.spec` will be flagged when `Name: hello`, even though the
//! shell would resolve the path successfully. This is intentional:
//! `fedpkg`/`koji`/`rpmbuild` workflows are case-sensitive regardless
//! of the developer's checkout filesystem, and silently masking the
//! mismatch on macOS would let a wrong name slip into the source RPM.

use std::path::{Path, PathBuf};

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{package_name, spec_span};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM312",
    name: "spec-filename-mismatch",
    description: "Spec file name differs from `<Name>.spec` — most RPM tooling pairs specs to \
                  package names by filename.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct SpecFilenameMismatch {
    diagnostics: Vec<Diagnostic>,
    source_path: Option<PathBuf>,
}

impl SpecFilenameMismatch {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SpecFilenameMismatch {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(path) = self.source_path.as_ref() else {
            return;
        };
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            return;
        };
        let Some(name) = package_name(spec) else {
            return;
        };
        let expected = format!("{name}.spec");
        if file_name != expected {
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "spec file is named `{file_name}` but `Name:` is `{name}`; expected \
                     `{expected}` so RPM tooling can pair them by filename"
                ),
                spec_span(spec),
            ));
        }
    }
}

impl Lint for SpecFilenameMismatch {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source_path(&mut self, path: Option<&std::path::Path>) {
        self.source_path = path.map(Path::to_path_buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str, path: Option<&Path>) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SpecFilenameMismatch::new();
        lint.set_source_path(path);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_filename_mismatch() {
        let diags = run("Name: hello\n", Some(Path::new("/tmp/world.spec")));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM312");
        assert!(diags[0].message.contains("hello.spec"));
        assert!(diags[0].message.contains("world.spec"));
    }

    #[test]
    fn silent_when_filename_matches() {
        assert!(run("Name: hello\n", Some(Path::new("/tmp/hello.spec"))).is_empty());
    }

    #[test]
    fn silent_when_no_path() {
        // stdin / in-memory: skip the check.
        assert!(run("Name: hello\n", None).is_empty());
    }

    #[test]
    fn silent_when_name_missing() {
        assert!(run("Version: 1\n", Some(Path::new("/tmp/world.spec"))).is_empty());
    }

    #[test]
    fn silent_when_name_is_macro() {
        // `Name: %{base_name}` — can't compare.
        assert!(run("Name: %{base_name}\n", Some(Path::new("/tmp/world.spec"))).is_empty());
    }
}
