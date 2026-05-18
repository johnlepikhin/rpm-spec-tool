//! RPM303 `release-disttag-policy` — `Release:` policy for Fedora-family
//! distros.
//!
//! Fedora/RHEL packages must reference `%{?dist}` in `Release:` so the
//! same spec yields distinct NVRs across distro generations
//! (`1.fc40`, `1.el9`, …). The rule fires on the two main violations:
//!
//! 1. `Release:` does not contain `%{?dist}` at all.
//! 2. `Release:` hard-codes a distro suffix (`.fc40`, `.el9`, `.mga9`),
//!    bypassing the macro indirection. Mass rebuilds (Fedora's "rawhide
//!    → branched" branch swaps) re-derive the dist tag per target; a
//!    hardcoded `.fc40` becomes wrong the moment the build is run on
//!    Fedora 41 / EPEL 9 / RHEL 9.
//!
//! Family-gated via `applies_to_profile` so non-Fedora distros (ALT,
//! openSUSE) stay silent regardless of severity overrides.

use rpm_spec::ast::{Span, SpecFile, Tag, TagValue, Text, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::policy::{DistTagPolicy, PolicyRegistry};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;
use rpm_spec_profile::Profile;

// `Default` on PolicyRegistry yields the silent generic table, so
// `derive(Default)` on the rule struct produces a zero-arg constructor
// that mirrors what the other Phase 20 rules use.

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM303",
    name: "release-disttag-policy",
    description: "`Release:` should reference `%{?dist}` and not hard-code a per-distro \
                  suffix (`.fc40`, `.el9`, ...). Family-gated to Fedora-derived distros.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// `Release:` should reference `%{?dist}` and not hard-code a per-distro suffix (`.fc40`, `.el9`, ...). Family-gated to Fedora-derived distros.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ReleaseDisttagPolicy {
    diagnostics: Vec<Diagnostic>,
    policy: PolicyRegistry,
}

impl ReleaseDisttagPolicy {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ReleaseDisttagPolicy {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let policy = self.policy;
        // `applies_to_profile` already filters Generic away, but on
        // hermetic call paths (custom Profile in tests) the policy
        // may still be the silent baseline — bail explicitly.
        if matches!(policy.disttag, DistTagPolicy::NotApplicable)
            && policy.hardcoded_dist_substrings.is_empty()
        {
            return;
        }
        for item in collect_top_level_preamble(spec) {
            if !matches!(item.tag, Tag::Release) {
                continue;
            }
            let TagValue::Text(t) = &item.value else {
                continue;
            };
            // Hardcoded dist suffix check: scan literal segments.
            if let Some(suffix) = first_hardcoded_dist(t, policy.hardcoded_dist_substrings) {
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`Release:` hard-codes `{suffix}`; use `%{{?dist}}` so the suffix \
                         tracks the build target"
                    ),
                    item.data,
                ));
                continue;
            }
            if matches!(policy.disttag, DistTagPolicy::Required) && !references_dist_macro(t) {
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "`Release:` is missing `%{?dist}`; Fedora/RHEL conventions require it for \
                     NVR uniqueness across targets",
                    item.data,
                ));
            }
        }
    }
}

fn references_dist_macro(t: &Text) -> bool {
    t.segments.iter().any(|seg| match seg {
        TextSegment::Macro(m) => m.name == "dist",
        _ => false,
    })
}

fn first_hardcoded_dist(t: &Text, needles: &[&str]) -> Option<String> {
    if needles.is_empty() {
        return None;
    }
    let mut buf = String::new();
    for seg in &t.segments {
        if let TextSegment::Literal(s) = seg {
            buf.push_str(s);
        }
    }
    for needle in needles {
        if let Some(pos) = buf.find(needle) {
            // Return the matched substring up to the next dot or
            // end-of-string for readability.
            let tail = &buf[pos..];
            let end = tail
                .char_indices()
                .skip(1)
                .find(|(_, c)| *c == '.' || c.is_whitespace())
                .map(|(i, _)| i)
                .unwrap_or(tail.len());
            return Some(tail[..end].to_owned());
        }
    }
    None
}

impl Lint for ReleaseDisttagPolicy {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn applies_to_profile(&self, profile: &Profile) -> bool {
        let policy = PolicyRegistry::for_profile(profile);
        // Only run when the family enforces some dist-tag rule or
        // explicitly lists hardcoded substrings to flag.
        !matches!(policy.disttag, DistTagPolicy::NotApplicable)
            || !policy.hardcoded_dist_substrings.is_empty()
    }

    fn set_profile(&mut self, profile: &Profile) {
        self.policy = PolicyRegistry::for_profile(profile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_profile::{Family, Profile};

    fn fedora_profile() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Fedora);
        p
    }

    fn opensuse_profile() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Opensuse);
        p
    }

    fn mageia_profile() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Mageia);
        p
    }

    fn run(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = crate::session::parse(src);
        let mut lint = ReleaseDisttagPolicy::new();
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_release_without_dist_on_fedora() {
        let src = "Name: x\nVersion: 1\nRelease: 1\n";
        let diags = run(src, &fedora_profile());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM303");
        assert!(diags[0].message.contains("missing"));
    }

    #[test]
    fn silent_for_release_with_dist_on_fedora() {
        let src = "Name: x\nVersion: 1\nRelease: 1%{?dist}\n";
        assert!(run(src, &fedora_profile()).is_empty());
    }

    #[test]
    fn flags_hardcoded_fc_dist() {
        let src = "Name: x\nVersion: 1\nRelease: 1.fc40\n";
        let diags = run(src, &fedora_profile());
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains(".fc40"));
    }

    #[test]
    fn flags_hardcoded_el_dist() {
        let src = "Name: x\nVersion: 1\nRelease: 1.el9\n";
        let diags = run(src, &fedora_profile());
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains(".el9"));
    }

    #[test]
    fn flags_hardcoded_mga_dist_on_mageia() {
        // Mageia: DistTagPolicy::NotApplicable + ".mga" needle, so
        // missing `%{?dist}` is silent but a hardcoded `.mga9` fires.
        let src = "Name: x\nVersion: 1\nRelease: 1.mga9\n";
        let diags = run(src, &mageia_profile());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains(".mga9"));
    }

    #[test]
    fn silent_for_release_without_dist_on_mageia() {
        // Mageia doesn't require `%{?dist}` — bare `Release: 1` is OK.
        let src = "Name: x\nVersion: 1\nRelease: 1\n";
        assert!(run(src, &mageia_profile()).is_empty());
    }

    #[test]
    fn silent_outside_fedora_family() {
        // Two-layer assertion:
        // 1. `applies_to_profile` returns false so the registry skips
        //    the rule entirely (the production path).
        // 2. Even if a future caller bypasses the gate, a freshly
        //    `set_profile`'d openSUSE run on a `Release: 1` spec must
        //    emit nothing.
        let profile = opensuse_profile();
        let lint = ReleaseDisttagPolicy::new();
        assert!(!lint.applies_to_profile(&profile));

        let src = "Name: x\nVersion: 1\nRelease: 1\n";
        let outcome = crate::session::parse(src);
        let mut lint = ReleaseDisttagPolicy::new();
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn silent_for_release_with_dist_macro_anywhere() {
        let src = "Name: x\nVersion: 1\nRelease: 1.git20240101%{?dist}\n";
        assert!(run(src, &fedora_profile()).is_empty());
    }
}
