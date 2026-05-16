//! RPM127 `legacy-license-syntax` — flag legacy license identifiers
//! (`GPLv2+`, `BSD`, `ASL 2.0`, …) on modern Fedora.
//!
//! ## Profile gating
//!
//! Fedora ≥ 40 mandates SPDX-only license identifiers in `License:`
//! tags. Pre-F40 Fedora and every other distro (RHEL clones, openSUSE,
//! ALT, …) still accept the legacy short forms, so flagging them
//! would be false-positive noise outside the modern-Fedora target.
//!
//! Gate: `applies_to_profile` returns `true` only when
//!   `family == Some(Family::Fedora)` AND `dist_tag` parses as `.fcN`
//!   with `N ≥ 40`. Other distros / older Fedora silently skip the
//!   rule entirely via the session-level filter.
//!
//! ## Trigger
//!
//! `License:` tag value (on the main package and every `%package`
//! subpackage) is split into SPDX-style atoms (`OR`/`AND`/`WITH`,
//! same logic as RPM024) and each atom is matched against a small
//! legacy-name table. Atoms containing macros (`%{?dist_license}`)
//! are skipped — can't see through them at lint time.

use rpm_spec::ast::{Span, Tag, TagValue};
use rpm_spec_profile::{Family, Profile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{iter_packages, split_spdx_atoms};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM127",
    name: "legacy-license-syntax",
    description: "Fedora ≥ 40 mandates SPDX-only license identifiers; legacy short forms \
                  (`GPLv2+`, `BSD`, …) are no longer accepted.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// Fedora release at which SPDX-only license identifiers became
/// mandatory (per the Fedora Legal change tracking the SPDX 3.x
/// adoption). Bump this when Fedora policy moves.
const FEDORA_SPDX_MIN_RELEASE: u32 = 40;

/// Subset of common legacy license names mapped to their SPDX
/// equivalents. The list is intentionally small — only the most
/// frequently-seen legacy spellings — so we get high signal on the
/// patterns Fedora reviewers actually call out. Comprehensive SPDX
/// validation belongs in RPM024 (invalid-license) with a populated
/// allowlist.
///
/// Scaling note: O(N) linear scan per license atom. Fine at the current
/// ~14 entries; if the table grows past ~50 switch to `phf::Map` or
/// pre-sort + binary search.
///
/// Source: derived from the legacy Fedora license short-names and the
/// Fedora `spdx-licenses-data` mapping (subset).
const LEGACY_TO_SPDX: &[(&str, &str)] = &[
    ("GPLv2", "GPL-2.0-only"),
    ("GPLv2+", "GPL-2.0-or-later"),
    ("GPLv3", "GPL-3.0-only"),
    ("GPLv3+", "GPL-3.0-or-later"),
    ("LGPLv2", "LGPL-2.0-only"),
    ("LGPLv2+", "LGPL-2.0-or-later"),
    ("LGPLv2.1", "LGPL-2.1-only"),
    ("LGPLv2.1+", "LGPL-2.1-or-later"),
    ("LGPLv3", "LGPL-3.0-only"),
    ("LGPLv3+", "LGPL-3.0-or-later"),
    ("BSD", "BSD-3-Clause"),
    ("ASL 2.0", "Apache-2.0"),
    ("Public Domain", "LicenseRef-Fedora-Public-Domain"),
    ("zlib", "Zlib"),
];

#[derive(Debug, Default)]
pub struct LegacyLicenseSyntax {
    diagnostics: Vec<Diagnostic>,
    family: Option<Family>,
    dist_tag: Option<String>,
}

impl LegacyLicenseSyntax {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_value(&mut self, value: &TagValue, span: Span) {
        let TagValue::Text(text) = value else { return };
        let Some(literal) = text.literal_str() else {
            return;
        };
        for atom in split_spdx_atoms(literal) {
            if let Some((_, spdx)) = LEGACY_TO_SPDX
                .iter()
                .find(|(legacy, _)| atom.eq_ignore_ascii_case(legacy))
            {
                let msg = format!(
                    "`{atom}` is the pre-SPDX legacy name — Fedora ≥ {FEDORA_SPDX_MIN_RELEASE} requires `{spdx}`"
                );
                self.diagnostics.push(
                    Diagnostic::new(&METADATA, METADATA.default_severity, msg, span)
                        .with_suggestion(Suggestion::new(
                            format!("replace `{atom}` with `{spdx}`"),
                            Vec::new(),
                            Applicability::Manual,
                        )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for LegacyLicenseSyntax {
    fn visit_spec(&mut self, spec: &'ast rpm_spec::ast::SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            for item in pkg.items() {
                if matches!(item.tag, Tag::License) {
                    self.check_value(&item.value, item.data);
                }
            }
        }
    }
}

impl Lint for LegacyLicenseSyntax {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }

    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn set_profile(&mut self, profile: &Profile) {
        self.family = profile.identity.family;
        self.dist_tag = profile.identity.dist_tag.clone();
    }

    fn applies_to_profile(&self, profile: &Profile) -> bool {
        // Family must be Fedora AND dist_tag must be `.fcN` with N ≥ 40.
        if !matches!(profile.identity.family, Some(Family::Fedora)) {
            return false;
        }
        match profile.identity.dist_tag.as_deref() {
            Some(tag) => fedora_release_at_least(tag, FEDORA_SPDX_MIN_RELEASE),
            None => false,
        }
    }
}

/// Parse a Fedora `dist_tag` (e.g. `.fc40`, `.fc41`) and return true
/// when the release number is ≥ `min`. Returns `false` for any tag
/// that doesn't match the `.fcN` pattern — including the `.el*` /
/// `.altN` / no-tag cases.
fn fedora_release_at_least(tag: &str, min: u32) -> bool {
    let Some(rest) = tag.strip_prefix(".fc") else {
        return false;
    };
    // Take leading digits only (`.fc40`, `.fc40+rc1` → 40).
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u32>().map(|n| n >= min).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::util::make_test_profile;
    use crate::session::parse;

    fn run(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = LegacyLicenseSyntax::new();
        lint.set_profile(profile);
        // Mimic session behaviour: skip visit when applies_to_profile is false.
        if !lint.applies_to_profile(profile) {
            return Vec::new();
        }
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn fires_on_fedora_40_legacy_gpl() {
        let profile = make_test_profile(Some(Family::Fedora), Some(".fc40"), &[], &[]);
        let diags = run("Name: x\nLicense: GPLv2+\n", &profile);
        assert_eq!(diags.len(), 1, "expected emit; got {diags:?}");
        assert_eq!(diags[0].lint_id, "RPM127");
        assert!(diags[0].message.contains("GPL-2.0-or-later"));
    }

    #[test]
    fn silent_on_fedora_39() {
        let profile = make_test_profile(Some(Family::Fedora), Some(".fc39"), &[], &[]);
        let diags = run("Name: x\nLicense: GPLv2+\n", &profile);
        assert!(
            diags.is_empty(),
            "F39 still accepts legacy names; got {diags:?}"
        );
    }

    #[test]
    fn silent_on_rhel() {
        let profile = make_test_profile(Some(Family::Rhel), Some(".el9"), &[], &[]);
        let diags = run("Name: x\nLicense: GPLv2+\n", &profile);
        assert!(
            diags.is_empty(),
            "RHEL: legacy names still allowed; got {diags:?}"
        );
    }

    #[test]
    fn silent_on_opensuse() {
        let profile = make_test_profile(Some(Family::Opensuse), None, &[], &[]);
        assert!(run("Name: x\nLicense: GPLv2+\n", &profile).is_empty());
    }

    #[test]
    fn silent_when_license_is_already_spdx() {
        let profile = make_test_profile(Some(Family::Fedora), Some(".fc41"), &[], &[]);
        let diags = run("Name: x\nLicense: GPL-2.0-or-later\n", &profile);
        assert!(diags.is_empty(), "SPDX name is fine; got {diags:?}");
    }

    #[test]
    fn flags_each_legacy_atom_in_spdx_expression() {
        // Two legacy atoms in one expression — both should be flagged.
        let profile = make_test_profile(Some(Family::Fedora), Some(".fc40"), &[], &[]);
        let diags = run("Name: x\nLicense: GPLv2+ OR LGPLv2+\n", &profile);
        assert_eq!(diags.len(), 2, "expected two diags; got {diags:?}");
    }

    #[test]
    fn skips_macro_bearing_value() {
        let profile = make_test_profile(Some(Family::Fedora), Some(".fc40"), &[], &[]);
        assert!(run("Name: x\nLicense: %{?dist_license}\n", &profile).is_empty());
    }

    #[test]
    fn fedora_release_parser() {
        assert!(fedora_release_at_least(".fc40", 40));
        assert!(fedora_release_at_least(".fc41", 40));
        assert!(!fedora_release_at_least(".fc39", 40));
        assert!(!fedora_release_at_least(".el9", 40));
        assert!(!fedora_release_at_least("", 40));
        // `.fc40+rc1` — take the leading digit run.
        assert!(fedora_release_at_least(".fc40+rc1", 40));
    }
}
