//! RPM025 `non-standard-group` — check that the `Group:` tag of every
//! package names a group from the profile's allow-list.
//!
//! ## Profile contract
//!
//! Silent when `profile.groups.mode == ValidationMode::Off` (contract
//! documented on `GroupList` in `rpm-spec-profile`). With `Warn` or
//! `Strict`, the literal group string is matched against
//! `profile.groups.allowed` and unknown values are flagged.
//!
//! ## Why a separate rule (not part of RPM024)
//!
//! Groups are no longer required by modern distros (Fedora dropped the
//! requirement in 2019; openSUSE still uses them) — keeping this as a
//! distinct lint lets users opt in via profile per distro target. The
//! `License:` rule (RPM024) is universally relevant; `Group:` is not.

use rpm_spec::ast::{Span, Tag, TagValue};
use rpm_spec_profile::{Profile, ValidationMode};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::iter_packages;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM025",
    name: "non-standard-group",
    description: "Group: must name a group from the profile's allow-list.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct NonStandardGroup {
    diagnostics: Vec<Diagnostic>,
    allowed: std::collections::BTreeSet<String>,
    mode: ValidationMode,
}

impl NonStandardGroup {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_value(&mut self, value: &TagValue, span: Span) {
        let TagValue::Text(text) = value else { return };
        let Some(literal) = text.literal_str() else {
            // Macro-bearing value — can't see through it.
            return;
        };
        let group = literal.trim();
        if group.is_empty() || self.allowed.contains(group) {
            return;
        }
        // Cheap nearest-neighbour hint: any allowed group that shares
        // the leading slash-segment (e.g. `System/Libraries` and
        // `System/Daemons` both root at `System`). Costs O(|allowed|)
        // per miss, but `allowed` is tiny.
        let hint = self.allowed.iter().find(|known| {
            let group_root = group.split('/').next().unwrap_or(group);
            let known_root = known.split('/').next().unwrap_or(known.as_str());
            !group_root.is_empty() && group_root == known_root
        });
        let msg = match hint {
            Some(neighbour) => format!(
                "group `{group}` is not in the profile allow-list — closest known: `{neighbour}`"
            ),
            None => format!(
                "group `{group}` is not in the profile allow-list \
                 ({n} entries); run `profile show --full` for the full set",
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

impl<'ast> Visit<'ast> for NonStandardGroup {
    fn visit_spec(&mut self, spec: &'ast rpm_spec::ast::SpecFile<Span>) {
        if matches!(self.mode, ValidationMode::Off) {
            return;
        }
        for pkg in iter_packages(spec) {
            for item in pkg.items() {
                if matches!(item.tag, Tag::Group) {
                    self.check_value(&item.value, item.data);
                }
            }
        }
    }
}

impl Lint for NonStandardGroup {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }

    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn set_profile(&mut self, profile: &Profile) {
        self.allowed = profile.groups.allowed.clone();
        self.mode = profile.groups.mode;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str, allowed: &[&str], mode: ValidationMode) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = NonStandardGroup::new();
        let mut profile = Profile::default();
        profile.groups.allowed = allowed.iter().map(|s| (*s).to_string()).collect();
        profile.groups.mode = mode;
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn silent_when_mode_is_off() {
        let diags = run(
            "Name: x\nGroup: Bogus/Thing\n",
            &["System/Libraries"],
            ValidationMode::Off,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn allows_listed_group() {
        let diags = run(
            "Name: x\nGroup: System/Libraries\n",
            &["System/Libraries", "Development/Tools"],
            ValidationMode::Warn,
        );
        assert!(diags.is_empty(), "got {diags:?}");
    }

    #[test]
    fn flags_unknown_group() {
        let diags = run(
            "Name: x\nGroup: Bogus/Thing\n",
            &["System/Libraries"],
            ValidationMode::Warn,
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM025");
        assert!(diags[0].message.contains("Bogus/Thing"));
    }

    #[test]
    fn suggests_neighbour_with_same_root() {
        // `System/Daemons` is unknown but shares `System/` with an
        // allowed peer — surface that as a hint.
        let diags = run(
            "Name: x\nGroup: System/Daemons\n",
            &["System/Libraries", "Development/Tools"],
            ValidationMode::Warn,
        );
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("System/Libraries"),
            "expected neighbour hint; got {}",
            diags[0].message
        );
    }

    #[test]
    fn skips_macro_bearing_value() {
        let diags = run(
            "Name: x\nGroup: %{?dist_group}\n",
            &["System/Libraries"],
            ValidationMode::Strict,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn checks_subpackages() {
        let src = "\
Name: x
Version: 1
Release: 1
License: MIT
Summary: s
Group: System/Libraries

%description
b

%package devel
Summary: d
Group: Bogus/Thing

%description devel
d
";
        let diags = run(src, &["System/Libraries"], ValidationMode::Warn);
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert!(diags[0].message.contains("Bogus/Thing"));
    }
}
