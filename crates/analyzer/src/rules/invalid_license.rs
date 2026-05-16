//! RPM024 `invalid-license` — check that the `License:` tag of every
//! package (main + each `%package` subpackage) names a license from the
//! profile's allow-list.
//!
//! ## Profile contract
//!
//! The rule is **silent** when `profile.licenses.mode == ValidationMode::Off`
//! (the contract documented on `LicenseList` in `rpm-spec-profile`). With
//! `Warn` or `Strict`, every literal license atom is matched against
//! `profile.licenses.allowed`; unknown atoms are flagged.
//!
//! ## SPDX-expression handling
//!
//! `License:` is an SPDX expression — operators `OR` / `AND` / `WITH`
//! and parentheses are legal. We split on those keywords (case-insensitive)
//! and trim surrounding brackets, then test each atom. This handles the
//! common forms (`MIT`, `MIT OR Apache-2.0`, `(MIT OR GPL-2.0-or-later)`,
//! `GPL-2.0-or-later WITH Classpath-exception-2.0`) without pulling in a
//! full SPDX parser. Atoms containing macros (`%{?fedora_license}`) are
//! skipped — we can't see through them at lint time.

use rpm_spec::ast::{Span, Tag, TagValue};
use rpm_spec_profile::{Profile, ValidationMode};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{iter_packages, split_spdx_atoms};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM024",
    name: "invalid-license",
    description: "License: must name a license from the profile's allow-list.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct InvalidLicense {
    diagnostics: Vec<Diagnostic>,
    /// Snapshot of the profile's license allow-list. Empty when the
    /// profile didn't set one — combined with `mode` below, that means
    /// the rule stays silent.
    allowed: std::collections::BTreeSet<String>,
    mode: ValidationMode,
}

impl InvalidLicense {
    pub fn new() -> Self {
        Self::default()
    }

    /// Examine one `License:` value and append diagnostics for every
    /// atom not in `self.allowed`.
    fn check_value(&mut self, value: &TagValue, span: Span) {
        let TagValue::Text(text) = value else { return };
        let Some(literal) = text.literal_str() else {
            // Macro-bearing value — can't see through it at lint time.
            return;
        };
        for atom in split_spdx_atoms(literal) {
            if atom.is_empty() {
                continue;
            }
            if !self.allowed.contains(atom) {
                let lowered = atom.to_ascii_lowercase();
                let suggestion = self
                    .allowed
                    .iter()
                    .find(|known| known.to_ascii_lowercase() == lowered);
                let msg = match suggestion {
                    Some(canonical) => format!(
                        "license `{atom}` is not in the profile allow-list — did you mean `{canonical}`?"
                    ),
                    None => format!(
                        "license `{atom}` is not in the profile allow-list (\
                         {n} entries); check `profile show --full` for the full set",
                        n = self.allowed.len()
                    ),
                };
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    METADATA.default_severity,
                    msg,
                    span,
                ));
            }
        }
    }
}

impl<'ast> Visit<'ast> for InvalidLicense {
    fn visit_spec(&mut self, spec: &'ast rpm_spec::ast::SpecFile<Span>) {
        // Contract: silent when the profile didn't opt in.
        if matches!(self.mode, ValidationMode::Off) {
            return;
        }
        for pkg in iter_packages(spec) {
            for item in pkg.items() {
                if matches!(item.tag, Tag::License) {
                    self.check_value(&item.value, item.data);
                }
            }
        }
    }
}

impl Lint for InvalidLicense {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }

    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn set_profile(&mut self, profile: &Profile) {
        self.allowed = profile.licenses.allowed.clone();
        self.mode = profile.licenses.mode;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str, allowed: &[&str], mode: ValidationMode) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = InvalidLicense::new();
        let mut profile = Profile::default();
        profile.licenses.allowed = allowed.iter().map(|s| (*s).to_string()).collect();
        profile.licenses.mode = mode;
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn silent_when_mode_is_off() {
        // Contract: even with a populated allow-list, mode=Off must not emit.
        let diags = run(
            "Name: x\nLicense: WTFPL\n",
            &["MIT", "GPL-2.0-or-later"],
            ValidationMode::Off,
        );
        assert!(diags.is_empty(), "expected silence; got {diags:?}");
    }

    #[test]
    fn allows_listed_license() {
        let diags = run(
            "Name: x\nLicense: MIT\n",
            &["MIT", "GPL-2.0-or-later"],
            ValidationMode::Warn,
        );
        assert!(diags.is_empty(), "MIT is allowed; got {diags:?}");
    }

    #[test]
    fn flags_unknown_license() {
        let diags = run(
            "Name: x\nLicense: WTFPL\n",
            &["MIT", "GPL-2.0-or-later"],
            ValidationMode::Warn,
        );
        assert_eq!(diags.len(), 1, "expected one diagnostic; got {diags:?}");
        assert_eq!(diags[0].lint_id, "RPM024");
        assert!(
            diags[0].message.contains("WTFPL"),
            "message should name the bad license; got {}",
            diags[0].message
        );
    }

    #[test]
    fn splits_spdx_or_and_with() {
        // `OR` + `WITH` — `Apache-2.0` allowed, `WTFPL` not. One flag.
        let diags = run(
            "Name: x\nLicense: WTFPL OR Apache-2.0 WITH Classpath-exception-2.0\n",
            &["MIT", "Apache-2.0", "Classpath-exception-2.0"],
            ValidationMode::Warn,
        );
        assert_eq!(diags.len(), 1, "only WTFPL is bad; got {diags:?}");
        assert!(diags[0].message.contains("WTFPL"));
    }

    #[test]
    fn handles_parens() {
        let diags = run(
            "Name: x\nLicense: (MIT OR Apache-2.0)\n",
            &["MIT", "Apache-2.0"],
            ValidationMode::Warn,
        );
        assert!(
            diags.is_empty(),
            "parenthesised group is fine; got {diags:?}"
        );
    }

    #[test]
    fn skips_macro_bearing_value() {
        // We can't see through macros — must not false-fire.
        let diags = run(
            "Name: x\nLicense: %{?dist_license}\n",
            &["MIT"],
            ValidationMode::Strict,
        );
        assert!(
            diags.is_empty(),
            "macro-bearing value must be skipped; got {diags:?}"
        );
    }

    #[test]
    fn case_insensitive_typo_hint() {
        let diags = run("Name: x\nLicense: mit\n", &["MIT"], ValidationMode::Warn);
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("did you mean `MIT`"),
            "case-mismatch should hint at canonical form; got {}",
            diags[0].message
        );
    }

    #[test]
    fn checks_subpackages() {
        let src = "\
Name: x
Version: 1
Release: 1
License: MIT
Summary: s

%description
b

%package devel
Summary: dev
License: WTFPL

%description devel
d
";
        let diags = run(src, &["MIT"], ValidationMode::Warn);
        assert_eq!(
            diags.len(),
            1,
            "subpackage's bad license must be flagged; got {diags:?}"
        );
        assert!(diags[0].message.contains("WTFPL"));
    }

    #[test]
    fn split_handles_word_boundary() {
        // `OR`/`AND` shouldn't match inside identifiers like `ORIGINAL`.
        let atoms = split_spdx_atoms("ORIGINAL");
        assert_eq!(atoms, vec!["ORIGINAL"]);
    }
}
