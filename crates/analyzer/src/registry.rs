//! Central registry of built-in lint rules.
//!
//! New rules: implement [`crate::Lint`] in `rules/<name>.rs`, expose a
//! `new()` constructor, and add a line to [`builtin_lints`].

use crate::lint::Lint;
use crate::rules;

/// Construct fresh instances of every built-in rule. Each call returns a new
/// `Vec` of independent `Box<dyn Lint>` objects; callers may then filter or
/// reorder them based on configuration.
pub fn builtin_lints() -> Vec<Box<dyn Lint>> {
    vec![
        // Phase 0 — proof-of-concept rules.
        Box::new(rules::missing_changelog::MissingChangelog::new()),
        Box::new(rules::empty_description::EmptyDescription::new()),
        // Phase 1 — packaging essentials.
        Box::new(rules::missing_tag::MissingNameTag::new()),
        Box::new(rules::missing_tag::MissingVersionTag::new()),
        Box::new(rules::missing_tag::MissingReleaseTag::new()),
        Box::new(rules::missing_tag::MissingLicenseTag::new()),
        Box::new(rules::missing_tag::MissingSummaryTag::new()),
        Box::new(rules::missing_tag::MissingUrlTag::new()),
        Box::new(rules::obsolete_tag::ObsoleteTag::new()),
        Box::new(rules::deprecated_clean_section::DeprecatedCleanSection::new()),
        Box::new(rules::multiple_changelog::MultipleChangelog::new()),
        // Phase 2 — correctness.
        Box::new(rules::requires_equal_version::RequiresEqualVersion::new()),
        Box::new(rules::macro_redefinition::MacroRedefinition::new()),
        Box::new(rules::self_obsoletion::SelfObsoletion::new()),
        Box::new(rules::obsolete_without_provides::ObsoleteWithoutProvides::new()),
        Box::new(rules::useless_explicit_provides::UselessExplicitProvides::new()),
        Box::new(rules::self_conflict::SelfConflict::new()),
    ]
}
