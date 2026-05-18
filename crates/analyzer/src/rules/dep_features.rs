//! Rpmlib-feature- and qualifier-aware dependency rules: RPM326,
//! RPM327.
//!
//! - **RPM326 `unsupported-dependency-feature`** — the spec uses a
//!   dependency feature (rich/boolean deps, weak deps, `Requires(meta)`
//!   qualifier) that the active profile's rpm doesn't advertise via
//!   `rpmlib(...)`. Such a spec parses cleanly with modern rpm but
//!   builds will fail on the target host.
//! - **RPM327 `contradictory-dependency-qualifiers`** — `meta` and
//!   ordered qualifiers (`pre`/`post`/`preun`/`postun`/`pretrans`/
//!   `posttrans`) on the same `Requires(...)` are mutually exclusive
//!   per rpm 4.16+ semantics. RPM keeps one and ignores the rest,
//!   silently; the typo is invisible at install time.
//!
//! RPM326 is profile-aware: it reads
//! [`rpm_spec_profile::RpmlibFeatures`] and matches feature names
//! verbatim. When the profile carries no feature data (Generic), the
//! rule stays silent rather than guess what rpm the target ships.

use rpm_spec::ast::{DepExpr, PreambleItem, Span, SpecFile, Tag, TagQualifier, TagValue};
use rpm_spec_profile::Profile;

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;

// =====================================================================
// RPM326 unsupported-dependency-feature
// =====================================================================

pub static UNSUPPORTED_FEATURE_METADATA: LintMetadata = LintMetadata {
    id: "RPM326",
    name: "unsupported-dependency-feature",
    description: "The spec uses a dependency feature (rich/boolean deps, weak deps, \
                  `Requires(meta)` qualifier) that the active profile's rpm does not advertise \
                  via `rpmlib(...)`. Builds will fail on that target.",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

/// The spec uses a dependency feature (rich/boolean deps, weak deps, `Requires(meta)` qualifier) that the active profile's rpm does not advertise via `rpmlib(...)`. Builds will fail on that target.
///
/// See [`UNSUPPORTED_FEATURE_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct UnsupportedDependencyFeature {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl UnsupportedDependencyFeature {
    pub fn new() -> Self {
        Self::default()
    }

    fn supports(&self, feature: &str) -> bool {
        self.profile.rpmlib.features.contains_key(feature)
    }
}

const RICH_DEPS_FEATURE: &str = "rpmlib(RichDependencies)";
const META_DEPS_FEATURE: &str = "rpmlib(MetaDependencies)";

impl<'ast> Visit<'ast> for UnsupportedDependencyFeature {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Profile with no rpmlib features set — usually the synthetic
        // generic profile in tests — gives us no signal to compare
        // against. Stay silent.
        if self.profile.rpmlib.features.is_empty() {
            return;
        }
        for item in collect_top_level_preamble(spec) {
            let TagValue::Dep(expr) = &item.value else {
                continue;
            };
            // Rich (boolean) deps require `rpmlib(RichDependencies)`.
            if contains_rich(expr) && !self.supports(RICH_DEPS_FEATURE) {
                self.diagnostics.push(Diagnostic::new(
                    &UNSUPPORTED_FEATURE_METADATA,
                    Severity::Deny,
                    "rich/boolean dependency expression used but the active profile's rpm does \
                     not advertise `rpmlib(RichDependencies)`",
                    item.data,
                ));
            }
            // `Requires(meta)` requires `rpmlib(MetaDependencies)`.
            if has_meta_qualifier(item) && !self.supports(META_DEPS_FEATURE) {
                self.diagnostics.push(Diagnostic::new(
                    &UNSUPPORTED_FEATURE_METADATA,
                    Severity::Deny,
                    "`Requires(meta)` used but the active profile's rpm does not advertise \
                     `rpmlib(MetaDependencies)`",
                    item.data,
                ));
            }
        }
    }
}

fn contains_rich(expr: &DepExpr) -> bool {
    // The parser normalises every boolean dependency expression
    // (`(foo and bar)`, `(foo if bar)`, ...) into the top-level
    // `DepExpr::Rich` variant — a nested rich expression inside an
    // `Atom` is structurally unrepresentable. So a top-level
    // `matches!` is sufficient.
    matches!(expr, DepExpr::Rich(_))
}

fn has_meta_qualifier(item: &PreambleItem<Span>) -> bool {
    item.qualifiers
        .iter()
        .any(|q| matches!(q, TagQualifier::Meta))
}

impl Lint for UnsupportedDependencyFeature {
    fn metadata(&self) -> &'static LintMetadata {
        &UNSUPPORTED_FEATURE_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
    }
}

// =====================================================================
// RPM327 contradictory-dependency-qualifiers
// =====================================================================

pub static CONTRADICTORY_QUALIFIERS_METADATA: LintMetadata = LintMetadata {
    id: "RPM327",
    name: "contradictory-dependency-qualifiers",
    description: "`Requires(meta, …)` combines the `meta` qualifier with ordered phase \
                  qualifiers (`pre`/`post`/`preun`/`postun`/`pretrans`/`posttrans`). The pair \
                  is contradictory; rpm silently keeps one.",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

/// `Requires(meta, …)` combines the `meta` qualifier with ordered phase qualifiers (`pre`/`post`/`preun`/`postun`/`pretrans`/`posttrans`). The pair is contradictory; rpm silently keeps one.
///
/// See [`CONTRADICTORY_QUALIFIERS_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ContradictoryDependencyQualifiers {
    diagnostics: Vec<Diagnostic>,
}

impl ContradictoryDependencyQualifiers {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Return the canonical lowercase label for an ordered phase
/// qualifier, or `None` if `q` is not one of the eight phase
/// variants. `TagQualifier::Meta`, `Verify`, `Interp`, `Other(_)`
/// and any future variant all return `None` — RPM327 only cares
/// about phase qualifiers, and a misleading "other" / "?" label
/// in the diagnostic message would be worse than skipping.
fn phase_qualifier_label(q: &TagQualifier) -> Option<&'static str> {
    Some(match q {
        TagQualifier::Pre => "pre",
        TagQualifier::Post => "post",
        TagQualifier::Preun => "preun",
        TagQualifier::Postun => "postun",
        TagQualifier::Pretrans => "pretrans",
        TagQualifier::Posttrans => "posttrans",
        TagQualifier::Preuntrans => "preuntrans",
        TagQualifier::Postuntrans => "postuntrans",
        _ => return None,
    })
}

impl<'ast> Visit<'ast> for ContradictoryDependencyQualifiers {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in collect_top_level_preamble(spec) {
            // Only the dependency-bearing tags can carry qualifiers
            // we care about; in practice that's `Requires`.
            if !matches!(item.tag, Tag::Requires) {
                continue;
            }
            let has_meta = item
                .qualifiers
                .iter()
                .any(|q| matches!(q, TagQualifier::Meta));
            if !has_meta {
                continue;
            }
            let phase_label = item.qualifiers.iter().find_map(phase_qualifier_label);
            if let Some(label) = phase_label {
                self.diagnostics.push(Diagnostic::new(
                    &CONTRADICTORY_QUALIFIERS_METADATA,
                    Severity::Deny,
                    format!(
                        "`Requires(meta, {label})` mixes `meta` with a phase qualifier; rpm \
                         keeps only one — drop `meta` or remove the phase"
                    ),
                    item.data,
                ));
            }
        }
    }
}

impl Lint for ContradictoryDependencyQualifiers {
    fn metadata(&self) -> &'static LintMetadata {
        &CONTRADICTORY_QUALIFIERS_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;
    use rpm_spec_profile::{Profile, RpmlibFeatures};

    fn old_rpm_profile() -> Profile {
        // No features advertised — but also not empty: stub one
        // unrelated feature so the rule doesn't bail early via
        // `features.is_empty()`. This matches the production shape
        // where every distro showrc dump carries dozens of features.
        let mut p = Profile::default();
        let mut rl = RpmlibFeatures::default();
        rl.features
            .insert("rpmlib(PartialHardlinkSets)".into(), "4.0.4-1".into());
        p.rpmlib = rl;
        p
    }

    fn modern_rpm_profile() -> Profile {
        let mut p = Profile::default();
        let mut rl = RpmlibFeatures::default();
        rl.features
            .insert("rpmlib(RichDependencies)".into(), "4.12.0-1".into());
        rl.features
            .insert("rpmlib(MetaDependencies)".into(), "4.16.0-1".into());
        p.rpmlib = rl;
        p
    }

    fn run_326(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = UnsupportedDependencyFeature::new();
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_327(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ContradictoryDependencyQualifiers::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM326 -----

    #[test]
    fn rpm326_flags_rich_on_old_rpm() {
        let src = "Name: x\nRequires: (foo and bar)\n";
        let diags = run_326(src, &old_rpm_profile());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM326");
        assert!(diags[0].message.contains("RichDependencies"));
    }

    #[test]
    fn rpm326_silent_on_modern_rpm() {
        let src = "Name: x\nRequires: (foo and bar)\n";
        assert!(run_326(src, &modern_rpm_profile()).is_empty());
    }

    #[test]
    fn rpm326_silent_for_plain_atom_on_old_rpm() {
        let src = "Name: x\nRequires: foo\n";
        assert!(run_326(src, &old_rpm_profile()).is_empty());
    }

    #[test]
    fn rpm326_silent_when_features_empty() {
        // Generic profile / no showrc — rule has no signal.
        let mut p = Profile::default();
        p.rpmlib = RpmlibFeatures::default();
        let src = "Name: x\nRequires: (foo and bar)\n";
        assert!(run_326(src, &p).is_empty());
    }

    #[test]
    fn rpm326_flags_meta_qualifier_on_old_rpm() {
        let src = "Name: x\nRequires(meta): foo\n";
        let diags = run_326(src, &old_rpm_profile());
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("MetaDependencies"));
    }

    // ----- RPM327 -----

    #[test]
    fn rpm327_flags_meta_plus_pre() {
        let src = "Name: x\nRequires(pre,meta): foo\n";
        let diags = run_327(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM327");
        assert!(diags[0].message.contains("pre"));
    }

    #[test]
    fn rpm327_flags_meta_plus_post() {
        let src = "Name: x\nRequires(post,meta): foo\n";
        let diags = run_327(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("post"));
    }

    #[test]
    fn rpm327_silent_for_meta_alone() {
        let src = "Name: x\nRequires(meta): foo\n";
        assert!(run_327(src).is_empty());
    }

    #[test]
    fn rpm327_silent_for_pre_alone() {
        let src = "Name: x\nRequires(pre): foo\n";
        assert!(run_327(src).is_empty());
    }

    #[test]
    fn rpm327_flags_pre_post_meta_triple() {
        // 3-arity: `Requires(pre,post,meta)` — `meta` contradicts both
        // phase qualifiers. One diagnostic per item (first phase wins
        // for the message).
        let src = "Name: x\nRequires(pre,post,meta): foo\n";
        let diags = run_327(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm326_flags_combined_rich_and_meta() {
        // Both flavours unsupported on the same old-rpm profile —
        // each yields its own Deny.
        let src = "Name: x\nRequires: (foo and bar)\nRequires(meta): baz\n";
        let diags = run_326(src, &old_rpm_profile());
        assert_eq!(diags.len(), 2, "{diags:?}");
    }
}
