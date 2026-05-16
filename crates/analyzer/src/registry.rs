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
        // Phase 5 — modernization.
        Box::new(rules::deprecated_commands::WordScanLint::new(
            &rules::deprecated_commands::SETUP_TEST_METADATA,
            rules::deprecated_commands::SETUP_TEST_NEEDLES,
        )),
        Box::new(rules::deprecated_commands::WordScanLint::new(
            &rules::deprecated_commands::SETUP_INSTALL_METADATA,
            rules::deprecated_commands::SETUP_INSTALL_NEEDLES,
        )),
        Box::new(rules::deprecated_commands::WordScanLint::new(
            &rules::deprecated_commands::EGREP_FGREP_METADATA,
            rules::deprecated_commands::EGREP_FGREP_NEEDLES,
        )),
        Box::new(rules::setup_flags::SetupWithoutQFlag::new()),
        Box::new(rules::patch_tracking::PatchDefinedNotApplied::new()),
        // Phase 6 — conditional-block lints.
        Box::new(rules::conditional_structure::DeepConditionalNesting::new()),
        Box::new(rules::conditional_structure::UnreachableElifBranch::new()),
        Box::new(rules::conditional_structure::EmptyConditionalBranch::new()),
        Box::new(rules::conditional_structure::IfarchEmptyList::new()),
        Box::new(rules::conditional_simplify::ConstantCondition::new()),
        Box::new(rules::conditional_simplify::IdenticalConditionalBranches::new()),
        Box::new(rules::conditional_simplify::RedundantNestedCondition::new()),
        Box::new(rules::conditional_merge::AdjacentMergeableConditionals::new()),
        // Phase 7 — conditional optimisation.
        Box::new(rules::conditional_optimize::NestedAndCollapse::new()),
        Box::new(rules::conditional_optimize::EmptyElseDrop::new()),
        Box::new(rules::conditional_optimize::InvertEmptyIfArch::new()),
        Box::new(rules::conditional_optimize::ConstantTautologyInExpr::new()),
        Box::new(rules::conditional_optimize::DoubleNegationInExpr::new()),
        // Phase 7b — extended conditional lints.
        Box::new(rules::conditional_structure::SingleCommentOnlyBranch::new()),
        Box::new(rules::conditional_structure::IfarchNoarch::new()),
        Box::new(rules::conditional_structure::DuplicateArchInList::new()),
        Box::new(rules::conditional_structure::ConditionalCyclomaticComplexity::new()),
        Box::new(rules::conditional_optimize::CollapseElifIntoElse::new()),
        Box::new(rules::conditional_optimize::IdempotentInExpr::new()),
        Box::new(rules::conditional_optimize::SelfComparisonInExpr::new()),
        Box::new(rules::conditional_optimize::LineContinuationInCondition::new()),
        Box::new(rules::conditional_merge::IfNotXAfterIfX::new()),
        Box::new(rules::conditional_factoring::ConditionMentionedManyTimes::new()),
        Box::new(rules::conditional_idioms::PreferBcondForBuildOptions::new()),
        Box::new(rules::conditional_idioms::IfOnlyBuildRequires::new()),
        // Phase 7c — multi-branch refactoring.
        Box::new(rules::conditional_hoist::HoistCommonPrefix::new()),
        Box::new(rules::conditional_hoist::HoistCommonSuffix::new()),
        Box::new(rules::conditional_merge::MergeElifSameBody::new()),
        Box::new(rules::conditional_optimize::CollapseElseIfIntoElif::new()),
        Box::new(rules::conditional_optimize::AbsorptionInExpr::new()),
        // Phase 7d — interval analysis + anti-patterns.
        Box::new(rules::conditional_intervals::InequalityRedundancy::new()),
        Box::new(rules::conditional_intervals::InequalityContradiction::new()),
        Box::new(rules::conditional_optimize::StringSetRedundancy::new()),
        Box::new(rules::conditional_optimize::InvertedIfElse::new()),
        Box::new(rules::conditional_idioms::ConditionalBuildArch::new()),
        Box::new(rules::conditional_idioms::ConditionalNameTag::new()),
        // Phase 8a — boolean DNF normalisation.
        Box::new(rules::boolean_dnf::BooleanDnfRedundancy::new()),
        Box::new(rules::boolean_dnf::BooleanTautologyByCubes::new()),
        Box::new(rules::boolean_dnf::BooleanContradictionByCubes::new()),
        // Phase 8b — path-condition engine.
        Box::new(rules::unreachable_branch::UnreachableBranch::new()),
        Box::new(rules::dead_elif::DeadElif::new()),
        Box::new(rules::always_true_branch::AlwaysTrueBranch::new()),
        Box::new(rules::exhaustive_chain::ExhaustiveChain::new()),
        // Phase 8c — macro value propagation.
        Box::new(rules::macro_propagation::MacroFoldsIfTrivial::new()),
        Box::new(rules::macro_propagation::UnusedConditionalGlobal::new()),
        // Phase 9 — tree-level hoisting.
        Box::new(rules::leaf_hoist::CommonLeafLineHoistable::new()),
        // Phase 10 — shell-command modernization.
        Box::new(rules::shell_modernization::MakeWithoutMakeBuild::new()),
        Box::new(rules::shell_modernization::MakeInstallWithoutMakeInstall::new()),
        Box::new(rules::shell_modernization::ConfigureWithoutConfigureMacro::new()),
        // Phase 11 — subpackage hygiene.
        Box::new(rules::subpackage_hygiene::PackageWithoutDescription::new()),
        Box::new(rules::subpackage_hygiene::PackageWithoutFiles::new()),
        // Phase 12 — source URL + description style.
        Box::new(rules::source_style::SourceWithoutUrl::new()),
        Box::new(rules::source_style::DescriptionLeadsWithThisPackage::new()),
        // Phase 13 — shellcheck integration.
        Box::new(rules::shellcheck::ShellcheckLint::new()),
        // Phase 14 — profile-aware lints (silent unless the profile
        // sets a non-Off ValidationMode).
        Box::new(rules::invalid_license::InvalidLicense::new()),
        Box::new(rules::non_standard_group::NonStandardGroup::new()),
        // Phase 15 — family-gated rules (emit/no-emit polarity gated
        // via Lint::applies_to_profile; each rule is silent on distros
        // it doesn't target).
        Box::new(rules::legacy_license_syntax::LegacyLicenseSyntax::new()),
        Box::new(rules::group_tag_required_on_suse::GroupTagRequiredOnSuse::new()),
        Box::new(rules::bcond_on_non_fedora::BcondOnNonFedora::new()),
        // Phase 17 — metadata / cross-tag consistency.
        Box::new(rules::duplicate_singleton_tag::DuplicateSingletonTag::new()),
        Box::new(rules::subpackage_name_collision::SubpackageNameCollision::new()),
        Box::new(rules::nvre_format::InvalidNvreFormat::new()),
        Box::new(rules::source_version_consistency::SourceVersionMismatch::new()),
        Box::new(rules::source_patch_list::SourcePatchListMixing::new()),
        Box::new(rules::patch_tracking::PatchAppliedMoreThanOnce::new()),
        Box::new(rules::autoreqprov_comment::AutoreqprovWithoutComment::new()),
        Box::new(rules::buildarch_reparse::BuildarchReparseHazard::new()),
        Box::new(rules::arch_policy::ArchPolicyContradiction::new()),
        Box::new(rules::changelog_health::ChangelogOrderWeekdayEvr::new()),
        Box::new(rules::spec_filename::SpecFilenameMismatch::new()),
        // Phase 18 — `%files` rules built on FilesClassifier.
        Box::new(rules::files_config::EtcFileNotConfig::new()),
        Box::new(rules::files_config::ConfigUnderUsr::new()),
        Box::new(rules::files_config::PlainConfigWithoutComment::new()),
        Box::new(rules::files_license::LicenseFileMarkedDoc::new()),
        Box::new(rules::files_devel::DevelFileInNonDevelPackage::new()),
        Box::new(rules::files_locale::LocaleFileNotLang::new()),
        Box::new(rules::files_duplicate::DuplicateFilesInFilesSections::new()),
        Box::new(rules::files_standard::StandardDirOwned::new()),
        Box::new(rules::files_standard::BroadFilesGlob::new()),
        Box::new(rules::files_volatile::VarRunVarLockNotGhost::new()),
        Box::new(rules::files_attr::SuspiciousAttrPermissions::new()),
        Box::new(rules::files_debuginfo::DebuginfoPathInMainFiles::new()),
        // Phase 19 — scriptlet/install rules built on CommandUseIndex.
        Box::new(rules::scriptlet_health::ScriptletExitNotGuaranteedZero::new()),
        Box::new(rules::scriptlet_health::ScriptletUpgradeTestEqTwo::new()),
        Box::new(rules::scriptlet_commands::DirectSystemctlInScriptlet::new()),
        Box::new(rules::scriptlet_commands::ScriptletStateOutsideRpmState::new()),
        Box::new(rules::install_boundaries::InstallWritesOutsideBuildroot::new()),
        Box::new(rules::install_boundaries::RmRfBuildrootInInstall::new()),
        Box::new(rules::install_make::MakeinstallWithoutUnderscore::new()),
        Box::new(rules::install_make::MakeInstallMissingDestdir::new()),
        Box::new(rules::install_chown::InstallChownOrOwner::new()),
        // Phase 20 — PolicyRegistry-driven rules.
        Box::new(rules::release_disttag::ReleaseDisttagPolicy::new()),
        Box::new(rules::systemd_units::SystemdUnitWithoutHelperMacros::new()),
        Box::new(rules::systemd_units::SystemdUnitUnderEtcOrConfig::new()),
        Box::new(rules::ldconfig_style::LdconfigScriptletStyle::new()),
        Box::new(rules::tmpfiles_create::TmpfilesWithoutCreate::new()),
        Box::new(rules::users_groups::UnsafeUseraddGroupadd::new()),
        // Phase 21 — dependency semantics.
        Box::new(rules::dep_health::DuplicateDependencyAtom::new()),
        Box::new(rules::dep_health::WeakDepDuplicatesStrongDep::new()),
        Box::new(rules::dep_health::SelfWeakDependency::new()),
        Box::new(rules::dep_health::RuntimeRequiresLooksLikeBuildRequires::new()),
        Box::new(rules::dep_features::UnsupportedDependencyFeature::new()),
        Box::new(rules::dep_features::ContradictoryDependencyQualifiers::new()),
    ]
}
