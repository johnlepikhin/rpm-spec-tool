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
        // Phase 3 — sections.
        Box::new(rules::missing_section::MissingPrepSection::new()),
        Box::new(rules::missing_section::MissingBuildSection::new()),
        Box::new(rules::missing_section::MissingInstallSection::new()),
        Box::new(rules::duplicate_buildscript::DuplicateBuildscriptSection::new()),
        // Phase 3 — changelog health.
        Box::new(rules::changelog_health::EmptyChangelogEntry::new()),
        Box::new(rules::changelog_health::ChangelogFutureDate::new()),
        Box::new(rules::changelog_health::ChangelogImplausibleDate::new()),
        // Phase 4 — style / source-text.
        Box::new(rules::macro_in_hash_comment::MacroInHashComment::new()),
        Box::new(rules::hardcoded_paths::HardcodedPaths::new()),
        Box::new(rules::shell_vars::RpmBuildrootShellVar::new()),
        Box::new(rules::shell_vars::RpmSourceDirShellVar::new()),
        Box::new(rules::summary_style::SummaryEndsWithDot::new()),
        Box::new(rules::summary_style::SummaryNotCapitalized::new()),
        Box::new(rules::summary_style::SummaryTooLong::new()),
        Box::new(rules::summary_style::NameInSummary::new()),
        Box::new(rules::description_health::DescriptionShorterThanSummary::new()),
        Box::new(rules::tab_indent::TabIndent::new()),
        Box::new(rules::trailing_whitespace::TrailingWhitespace::new()),
    ]
}
