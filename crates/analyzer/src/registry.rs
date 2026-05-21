//! Central registry of built-in lint rules.
//!
//! New rules: implement [`crate::Lint`] in `rules/<name>.rs`, expose a
//! `new()` constructor, and add a line to the appropriate `phase*` helper
//! below.

use crate::lint::{Lint, LintMetadata};
use crate::rules;

/// Collect the [`LintMetadata`] of every built-in lint without keeping
/// the rule instances alive.
///
/// CLI commands like `lints` (which prints the rule reference) only need
/// `id`/`name`/`description`/`default_severity`/`category` — they don't
/// run the visitor. Returning `&'static LintMetadata` lets the caller
/// freely group/filter/sort without owning the heavy `Box<dyn Lint>`
/// trait objects.
///
/// Order follows [`builtin_lints`] (i.e. registration order); each
/// rule's [`Lint::additional_metadata`] is inlined right after the
/// rule's primary metadata so combined visitors (e.g. RPM-REPO-030 +
/// RPM-REPO-031 sharing one `UpgradeEvrCheck`) still surface every
/// emittable ID. Callers that want stable, category-grouped output
/// should sort the result themselves.
pub fn builtin_lint_metadata() -> Vec<&'static LintMetadata> {
    let mut out = Vec::new();
    for lint in builtin_lints() {
        out.push(lint.metadata());
        out.extend(lint.additional_metadata().iter().copied());
    }
    out
}

/// Construct fresh instances of every built-in rule. Each call returns a new
/// `Vec` of independent `Box<dyn Lint>` objects; callers may then filter or
/// reorder them based on configuration.
///
/// The implementation concatenates per-phase helper functions (`phase*`).
/// Adding a new rule: append a `Box::new(...)` line to the helper for the
/// appropriate phase, or introduce a new `phaseN_*()` helper and chain it
/// here. The total order (and hence registration order) is the
/// concatenation order below.
pub fn builtin_lints() -> Vec<Box<dyn Lint>> {
    phase0_proof_of_concept()
        .into_iter()
        .chain(phase1_packaging_essentials())
        .chain(phase2_correctness())
        .chain(phase3_sections())
        .chain(phase3_changelog_health())
        .chain(phase4_style_source_text())
        .chain(phase5_modernization())
        .chain(phase6_conditional_blocks())
        .chain(phase7_conditional_optimisation())
        .chain(phase7b_extended_conditional())
        .chain(phase7c_multi_branch_refactoring())
        .chain(phase7d_interval_analysis())
        .chain(phase8a_boolean_dnf())
        .chain(phase8b_path_condition_engine())
        .chain(phase8c_macro_propagation())
        .chain(phase9_tree_level_hoisting())
        .chain(phase10_shell_command_modernization())
        .chain(phase11_subpackage_hygiene())
        .chain(phase12_source_url_and_description_style())
        .chain(phase13_shellcheck())
        .chain(phase14_profile_aware())
        .chain(phase15_family_gated())
        .chain(phase17_metadata_cross_tag_consistency())
        .chain(phase18_files_classifier_rules())
        .chain(phase19_scriptlet_install_rules())
        .chain(phase20_policy_registry_driven())
        .chain(phase21_dependency_semantics())
        .chain(phase22_cross_section_dep_policy())
        .chain(phase23_build_install_policy())
        .chain(phase24_conditional_builds_macros())
        .chain(phase25_context_aware_condition_simplification())
        .chain(phase25_path_aware_item_dedup())
        .chain(phase25_prep_setup_patch_modernisation())
        .chain(phase25_macro_modernisation())
        .chain(phase25_files_cleanup())
        .chain(phase25_shell_body_cleanup())
        .chain(phase25_subpackage_style())
        .chain(phase25_rich_dependency_algebra())
        .chain(phase26_repo_aware())
        .collect()
}

// Repository-aware lints (RPM-REPO-*). Skip silently when the
// active profile has no repos / no cached metadata; the CLI's
// `matrix deps check` command surfaces a single one-time INFO note
// in that case.
fn phase26_repo_aware() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::repo::br_unresolvable::BuildRequiresUnresolvable::new()),
        Box::new(rules::repo::runtime_unresolvable::RuntimeRequiresUnresolvable::new()),
        Box::new(rules::repo::br_version_unsatisfied::BuildRequiresVersionUnsatisfied::new()),
        Box::new(rules::repo::missing_br_for_command::MissingBuildRequiresForCommand::new()),
        Box::new(rules::repo::missing_br_for_file::MissingBuildRequiresForFile::new()),
        Box::new(rules::repo::file_conflict::FileConflictWithExistingPackage::new()),
        // One instance emits both RPM-REPO-030 and RPM-REPO-031 from a
        // single visitor pass; the second metadata flows out via
        // `Lint::additional_metadata` so listings still see both IDs.
        Box::new(rules::repo::upgrade_check::UpgradeEvrCheck::new()),
    ]
}

// Phase 0 — proof-of-concept rules.
fn phase0_proof_of_concept() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::missing_changelog::MissingChangelog::new()),
        Box::new(rules::empty_description::EmptyDescription::new()),
    ]
}

// Phase 1 — packaging essentials.
fn phase1_packaging_essentials() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::missing_tag::MissingNameTag::new()),
        Box::new(rules::missing_tag::MissingVersionTag::new()),
        Box::new(rules::missing_tag::MissingReleaseTag::new()),
        Box::new(rules::missing_tag::MissingLicenseTag::new()),
        Box::new(rules::missing_tag::MissingSummaryTag::new()),
        Box::new(rules::missing_tag::MissingUrlTag::new()),
        Box::new(rules::obsolete_tag::ObsoleteTag::new()),
        Box::new(rules::deprecated_clean_section::DeprecatedCleanSection::new()),
        Box::new(rules::multiple_changelog::MultipleChangelog::new()),
    ]
}

// Phase 2 — correctness.
fn phase2_correctness() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::requires_equal_version::RequiresEqualVersion::new()),
        Box::new(rules::macro_redefinition::MacroRedefinition::new()),
        Box::new(rules::self_obsoletion::SelfObsoletion::new()),
        Box::new(rules::obsolete_without_provides::ObsoleteWithoutProvides::new()),
        Box::new(rules::useless_explicit_provides::UselessExplicitProvides::new()),
        Box::new(rules::self_conflict::SelfConflict::new()),
    ]
}

// Phase 3 — sections.
fn phase3_sections() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::missing_section::MissingPrepSection::new()),
        Box::new(rules::missing_section::MissingBuildSection::new()),
        Box::new(rules::missing_section::MissingInstallSection::new()),
        Box::new(rules::duplicate_buildscript::DuplicateBuildscriptSection::new()),
    ]
}

// Phase 3 — changelog health.
fn phase3_changelog_health() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::changelog_health::EmptyChangelogEntry::new()),
        Box::new(rules::changelog_health::ChangelogFutureDate::new()),
        Box::new(rules::changelog_health::ChangelogImplausibleDate::new()),
    ]
}

// Phase 4 — style / source-text.
fn phase4_style_source_text() -> Vec<Box<dyn Lint>> {
    vec![
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

// Phase 5 — modernization.
fn phase5_modernization() -> Vec<Box<dyn Lint>> {
    vec![
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
    ]
}

// Phase 6 — conditional-block lints.
fn phase6_conditional_blocks() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::conditional_structure::DeepConditionalNesting::new()),
        Box::new(rules::conditional_structure::UnreachableElifBranch::new()),
        Box::new(rules::conditional_structure::EmptyConditionalBranch::new()),
        Box::new(rules::conditional_structure::IfarchEmptyList::new()),
        Box::new(rules::conditional_simplify::ConstantCondition::new()),
        Box::new(rules::conditional_simplify::IdenticalConditionalBranches::new()),
        Box::new(rules::conditional_simplify::RedundantNestedCondition::new()),
        Box::new(rules::conditional_merge::AdjacentMergeableConditionals::new()),
    ]
}

// Phase 7 — conditional optimisation.
fn phase7_conditional_optimisation() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::conditional_optimize::NestedAndCollapse::new()),
        Box::new(rules::conditional_optimize::EmptyElseDrop::new()),
        Box::new(rules::conditional_optimize::InvertEmptyIfArch::new()),
        Box::new(rules::conditional_optimize::ConstantTautologyInExpr::new()),
        Box::new(rules::conditional_optimize::DoubleNegationInExpr::new()),
    ]
}

// Phase 7b — extended conditional lints.
fn phase7b_extended_conditional() -> Vec<Box<dyn Lint>> {
    vec![
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
    ]
}

// Phase 7c — multi-branch refactoring.
fn phase7c_multi_branch_refactoring() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::conditional_hoist::HoistCommonPrefix::new()),
        Box::new(rules::conditional_hoist::HoistCommonSuffix::new()),
        Box::new(rules::conditional_merge::MergeElifSameBody::new()),
        Box::new(rules::conditional_optimize::CollapseElseIfIntoElif::new()),
        Box::new(rules::conditional_optimize::AbsorptionInExpr::new()),
    ]
}

// Phase 7d — interval analysis + anti-patterns.
fn phase7d_interval_analysis() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::conditional_intervals::InequalityRedundancy::new()),
        Box::new(rules::conditional_intervals::InequalityContradiction::new()),
        Box::new(rules::conditional_optimize::StringSetRedundancy::new()),
        Box::new(rules::conditional_optimize::InvertedIfElse::new()),
        Box::new(rules::conditional_idioms::ConditionalBuildArch::new()),
        Box::new(rules::conditional_idioms::ConditionalNameTag::new()),
    ]
}

// Phase 8a — boolean DNF normalisation.
fn phase8a_boolean_dnf() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::boolean_dnf::BooleanDnfRedundancy::new()),
        Box::new(rules::boolean_dnf::BooleanTautologyByCubes::new()),
        Box::new(rules::boolean_dnf::BooleanContradictionByCubes::new()),
    ]
}

// Phase 8b — path-condition engine.
fn phase8b_path_condition_engine() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::unreachable_branch::UnreachableBranch::new()),
        Box::new(rules::dead_elif::DeadElif::new()),
        Box::new(rules::always_true_branch::AlwaysTrueBranch::new()),
        Box::new(rules::exhaustive_chain::ExhaustiveChain::new()),
    ]
}

// Phase 8c — macro value propagation.
fn phase8c_macro_propagation() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::macro_propagation::MacroFoldsIfTrivial::new()),
        Box::new(rules::macro_propagation::UnusedConditionalGlobal::new()),
    ]
}

// Phase 9 — tree-level hoisting.
fn phase9_tree_level_hoisting() -> Vec<Box<dyn Lint>> {
    vec![Box::new(rules::leaf_hoist::CommonLeafLineHoistable::new())]
}

// Phase 10 — shell-command modernization.
fn phase10_shell_command_modernization() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::shell_modernization::MakeWithoutMakeBuild::new()),
        Box::new(rules::shell_modernization::MakeInstallWithoutMakeInstall::new()),
        Box::new(rules::shell_modernization::ConfigureWithoutConfigureMacro::new()),
    ]
}

// Phase 11 — subpackage hygiene.
fn phase11_subpackage_hygiene() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::subpackage_hygiene::PackageWithoutDescription::new()),
        Box::new(rules::subpackage_hygiene::PackageWithoutFiles::new()),
    ]
}

// Phase 12 — source URL + description style.
fn phase12_source_url_and_description_style() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::source_style::SourceWithoutUrl::new()),
        Box::new(rules::source_style::DescriptionLeadsWithThisPackage::new()),
    ]
}

// Phase 13 — shellcheck integration.
fn phase13_shellcheck() -> Vec<Box<dyn Lint>> {
    vec![Box::new(rules::shellcheck::ShellcheckLint::new())]
}

// Phase 14 — profile-aware lints (silent unless the profile sets a non-Off
// ValidationMode).
fn phase14_profile_aware() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::invalid_license::InvalidLicense::new()),
        Box::new(rules::non_standard_group::NonStandardGroup::new()),
    ]
}

// Phase 15 — family-gated rules (emit/no-emit polarity gated via
// Lint::applies_to_profile; each rule is silent on distros it doesn't
// target).
fn phase15_family_gated() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::legacy_license_syntax::LegacyLicenseSyntax::new()),
        Box::new(rules::group_tag_required_on_suse::GroupTagRequiredOnSuse::new()),
        Box::new(rules::bcond_on_non_fedora::BcondOnNonFedora::new()),
    ]
}

// Phase 17 — metadata / cross-tag consistency.
fn phase17_metadata_cross_tag_consistency() -> Vec<Box<dyn Lint>> {
    vec![
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
    ]
}

// Phase 18 — `%files` rules built on FilesClassifier.
fn phase18_files_classifier_rules() -> Vec<Box<dyn Lint>> {
    vec![
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
    ]
}

// Phase 19 — scriptlet/install rules built on CommandUseIndex.
fn phase19_scriptlet_install_rules() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::scriptlet_health::ScriptletExitNotGuaranteedZero::new()),
        Box::new(rules::scriptlet_health::ScriptletUpgradeTestEqTwo::new()),
        Box::new(rules::scriptlet_commands::DirectSystemctlInScriptlet::new()),
        Box::new(rules::scriptlet_commands::ScriptletStateOutsideRpmState::new()),
        Box::new(rules::install_boundaries::InstallWritesOutsideBuildroot::new()),
        Box::new(rules::install_boundaries::RmRfBuildrootInInstall::new()),
        Box::new(rules::install_make::MakeinstallWithoutUnderscore::new()),
        Box::new(rules::install_make::MakeInstallMissingDestdir::new()),
        Box::new(rules::install_chown::InstallChownOrOwner::new()),
    ]
}

// Phase 20 — PolicyRegistry-driven rules.
fn phase20_policy_registry_driven() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::release_disttag::ReleaseDisttagPolicy::new()),
        Box::new(rules::systemd_units::SystemdUnitWithoutHelperMacros::new()),
        Box::new(rules::systemd_units::SystemdUnitUnderEtcOrConfig::new()),
        Box::new(rules::ldconfig_style::LdconfigScriptletStyle::new()),
        Box::new(rules::tmpfiles_create::TmpfilesWithoutCreate::new()),
        Box::new(rules::users_groups::UnsafeUseraddGroupadd::new()),
    ]
}

// Phase 21 — dependency semantics.
fn phase21_dependency_semantics() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::dep_health::DuplicateDependencyAtom::new()),
        Box::new(rules::dep_health::WeakDepDuplicatesStrongDep::new()),
        Box::new(rules::dep_health::SelfWeakDependency::new()),
        Box::new(rules::dep_health::RuntimeRequiresLooksLikeBuildRequires::new()),
        Box::new(rules::dep_features::UnsupportedDependencyFeature::new()),
        Box::new(rules::dep_features::ContradictoryDependencyQualifiers::new()),
    ]
}

// Phase 22 — cross-section dep policy.
fn phase22_cross_section_dep_policy() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::build_tool_brs::BuildToolUsedWithoutBuildRequires::new()),
        Box::new(rules::pkgconfig_br::PkgconfigFileWithoutPkgconfigBr::new()),
        Box::new(rules::scriptlet_deps::ScriptletCommandWithoutRequires::new()),
        Box::new(rules::patch_status_comment::PatchStatusCommentMissing::new()),
    ]
}

// Phase 23 — build/install policy.
fn phase23_build_install_policy() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::optflags::OptflagsOverridden::new()),
        Box::new(rules::werror::WerrorNotDisabled::new()),
        Box::new(rules::parallel_make::J1WithoutComment::new()),
        Box::new(rules::network_in_build::NetworkAccessInBuild::new()),
        Box::new(rules::disabled_check::DisabledCheckSection::new()),
        Box::new(rules::buildsystem_macros::BuildsystemMacroModernization::new()),
    ]
}

// Phase 24 — conditional builds / macros.
fn phase24_conditional_builds_macros() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::bcond_modern::PreferBcondNewSyntax::new()),
        Box::new(rules::bcond_usage::BcondDefinedButUnused::new()),
        Box::new(rules::bcond_usage::WithConditionWithoutBcond::new()),
        Box::new(rules::metadata_shell_macro::MacroShellExpansionInMetadata::new()),
        Box::new(rules::include_notice::IncludeNotExpanded::new()),
    ]
}

// Phase 25 — context-aware condition simplification (RPM430+).
fn phase25_context_aware_condition_simplification() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::context_redundant_part::ContextRedundantPart::new()),
        Box::new(rules::elif_history_simplify::ElifHistorySimplify::new()),
        Box::new(rules::condition_common_factor::ConditionCommonFactor::new()),
        Box::new(rules::condition_common_disjunct_factor::ConditionCommonDisjunctFactor::new()),
        Box::new(rules::unnecessary_condition_parens::UnnecessaryConditionParens::new()),
        Box::new(rules::negated_comparison_simplify::NegatedComparisonSimplify::new()),
        Box::new(rules::equality_chain_to_range::EqualityChainToRange::new()),
    ]
}

// Phase 25 — path-aware item dedup (RPM450+).
fn phase25_path_aware_item_dedup() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::guarded_item_already_unconditional::GuardedItemAlreadyUnconditional::new()),
        Box::new(rules::guarded_item_dominated::GuardedItemDominated::new()),
        Box::new(rules::complementary_guards_same_item::ComplementaryGuardsSameItem::new()),
        Box::new(rules::full_domain_conditional_item::FullDomainConditionalItem::new()),
        Box::new(rules::branch_item_subset::BranchItemSubset::new()),
        Box::new(rules::same_guard_clustering::SameGuardClustering::new()),
        Box::new(rules::repeated_ifelse_value_extraction::RepeatedIfelseValueExtraction::new()),
        Box::new(rules::same_body_different_conditions::SameBodyDifferentConditions::new()),
        Box::new(rules::adjacent_mutex_ifs::AdjacentMutexIfs::new()),
        Box::new(rules::same_body_arch_blocks::SameBodyArchBlocks::new()),
        Box::new(rules::arch_condition_domain_simplify::ArchConditionDomainSimplify::new()),
        Box::new(rules::arch_complement_shorter::ArchComplementShorter::new()),
        Box::new(rules::target_cpu_equality::TargetCpuEquality::new()),
        Box::new(rules::arch_subset_under_parent::ArchSubsetUnderParent::new()),
    ]
}

// Phase 25 — prep / setup / patch modernisation (RPM470+).
fn phase25_prep_setup_patch_modernisation() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::setup_autopatch_to_autosetup::SetupAutopatchToAutosetup::new()),
        Box::new(rules::patch_sequence_to_autopatch::PatchSequenceToAutopatch::new()),
        Box::new(rules::redundant_setup_default_name::RedundantSetupDefaultName::new()),
        Box::new(rules::redundant_cd_after_setup::RedundantCdAfterSetup::new()),
        Box::new(rules::manual_patch_command::ManualPatchCommand::new()),
        Box::new(rules::manual_tar_cd_to_setup::ManualTarCdToSetup::new()),
        Box::new(rules::manual_extra_source_unpack::ManualExtraSourceUnpack::new()),
        Box::new(rules::long_patch_list_to_patchlist::LongPatchListToPatchlist::new()),
    ]
}

// Phase 25 — macro modernisation (RPM436, RPM438, RPM490+).
fn phase25_macro_modernisation() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::bcond_negation_canonical::BcondNegationCanonical::new()),
        Box::new(rules::empty_optional_macro_arm::EmptyOptionalMacroArm::new()),
        Box::new(rules::optional_macro_boolean_shortening::OptionalMacroBooleanShortening::new()),
        Box::new(rules::no_op_conditional_macro::NoOpConditionalMacro::new()),
        Box::new(rules::macro_alias_of_builtin::MacroAliasOfBuiltin::new()),
        Box::new(rules::duplicate_macro_bodies::DuplicateMacroBodies::new()),
        Box::new(rules::macro_name_shadows_bcond_helper::MacroNameShadowsBcondHelper::new()),
        Box::new(rules::mixed_subpackage_reference_style::MixedSubpackageReferenceStyle::new()),
        Box::new(
            rules::redundant_subpackage_version_release::RedundantSubpackageVersionRelease::new(),
        ),
        Box::new(rules::repeated_subpackage_boilerplate::RepeatedSubpackageBoilerplate::new()),
        Box::new(rules::subpackage_description_copy::SubpackageDescriptionCopy::new()),
        Box::new(rules::description_equals_summary::DescriptionEqualsSummary::new()),
        Box::new(rules::commented_out_spec_code::CommentedOutSpecCode::new()),
        Box::new(rules::stale_disabled_source_or_patch::StaleDisabledSourceOrPatch::new()),
        Box::new(rules::excessive_section_separators::ExcessiveSectionSeparators::new()),
        Box::new(rules::canonical_major_section_order::CanonicalMajorSectionOrder::new()),
        Box::new(rules::preamble_tag_clustering::PreambleTagClustering::new()),
        Box::new(rules::repeated_comment_before_guards::RepeatedCommentBeforeGuards::new()),
        Box::new(rules::single_use_private_macro::SingleUsePrivateMacro::new()),
        Box::new(rules::unused_macro_parameter::UnusedMacroParameter::new()),
        Box::new(rules::macro_called_same_arg::MacroCalledSameArg::new()),
        Box::new(rules::long_literal_prefix::LongLiteralPrefix::new()),
    ]
}

// Phase 25 — %files cleanup (RPM510+).
fn phase25_files_cleanup() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::adjacent_doc_lines::AdjacentDocLines::new()),
        Box::new(rules::adjacent_license_lines::AdjacentLicenseLines::new()),
        Box::new(rules::redundant_default_defattr::RedundantDefaultDefattr::new()),
        Box::new(rules::files_directory_subsumes_child::FilesDirectorySubsumesChild::new()),
        Box::new(rules::files_glob_subsumes_explicit::FilesGlobSubsumesExplicit::new()),
        Box::new(rules::repeated_files_prefix::RepeatedFilesPrefix::new()),
        Box::new(rules::files_section_sort_blocks::FilesSectionSortBlocks::new()),
    ]
}

// Phase 25 — shell-body cleanup (RPM530+).
fn phase25_shell_body_cleanup() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::combine_install_dirs::CombineInstallDirs::new()),
        Box::new(rules::mkdir_install_to_install_d::MkdirInstallToInstallD::new()),
        Box::new(rules::cp_chmod_to_install_m::CpChmodToInstallM::new()),
        Box::new(rules::duplicate_shell_block::DuplicateShellBlock::new()),
        Box::new(rules::near_duplicate_shell_block::NearDuplicateShellBlock::new()),
        Box::new(rules::redundant_mkdir_before_install_d::RedundantMkdirBeforeInstallD::new()),
        Box::new(rules::repeated_rm_f::RepeatedRmF::new()),
        Box::new(rules::macro_composition_to_specific::MacroCompositionToSpecific::new()),
    ]
}

// Phase 25 — subpackage style (RPM550+).
fn phase25_subpackage_style() -> Vec<Box<dyn Lint>> {
    vec![Box::new(
        rules::prefer_relative_subpackage_name::PreferRelativeSubpackageName::new(),
    )]
}

// Phase 25 — rich dependency algebra (RPM590–RPM597).
fn phase25_rich_dependency_algebra() -> Vec<Box<dyn Lint>> {
    vec![
        Box::new(rules::richdep_idempotent::RichdepIdempotent::new()),
        Box::new(rules::richdep_singleton::RichdepSingleton::new()),
        Box::new(rules::richdep_absorption::RichdepAbsorption::new()),
        Box::new(rules::richdep_same_then_else::RichdepSameThenElse::new()),
        Box::new(rules::richdep_nested_flatten::RichdepNestedFlatten::new()),
        Box::new(rules::richdep_common_factor::RichdepCommonFactor::new()),
        Box::new(rules::dependency_constraint_subsumption::DependencyConstraintSubsumption::new()),
        Box::new(rules::guarded_dependency_subsumption::GuardedDependencySubsumption::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lock the total rule count so accidental drops/duplicates during
    /// refactoring of the per-phase helpers are caught immediately. Bump
    /// this when adding/removing rules. Counts `Lint` *instances* —
    /// combined rules like `UpgradeEvrCheck` (one instance, two metadata
    /// IDs) count as one; the full metadata count surfaced via
    /// [`builtin_lint_metadata`] is asserted separately.
    #[test]
    fn builtin_lints_contains_expected_count() {
        assert_eq!(builtin_lints().len(), 240);
    }

    /// `builtin_lint_metadata` must include every primary metadata plus
    /// every `additional_metadata` entry. Combined visitors (e.g.
    /// `UpgradeEvrCheck` emitting RPM-REPO-030 + RPM-REPO-031) would
    /// silently lose their second ID from listings if the registry
    /// helper regresses.
    #[test]
    fn builtin_lint_metadata_includes_additional_ids() {
        let ids: Vec<&str> = builtin_lint_metadata().iter().map(|m| m.id).collect();
        assert!(ids.contains(&"RPM-REPO-030"), "primary id missing: {ids:?}");
        assert!(
            ids.contains(&"RPM-REPO-031"),
            "additional metadata id missing from listing: {ids:?}",
        );
    }

    /// Every emittable lint id must be unique across the union of
    /// primary and `additional_metadata` entries. A duplicate id would
    /// silently break per-id severity overrides, SARIF rule lookups
    /// (which key on `lint.id`), and `rpm-spec-tool lints` listings
    /// (which would deduplicate or show ambiguous entries).
    #[test]
    fn builtin_lint_metadata_ids_are_globally_unique() {
        use std::collections::HashMap;
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for m in builtin_lint_metadata() {
            *counts.entry(m.id).or_default() += 1;
        }
        let duplicates: Vec<(&&str, &usize)> = counts.iter().filter(|(_, n)| **n > 1).collect();
        assert!(
            duplicates.is_empty(),
            "duplicate lint ids found in builtin_lint_metadata: {duplicates:?}",
        );
    }

    /// Per-phase helpers must round-trip into the same vector
    /// `builtin_lints()` produces. Catches helper-out-of-order regressions.
    #[test]
    fn builtin_lints_phase_helpers_concat_to_full() {
        let all = builtin_lints();
        let by_phase: Vec<Box<dyn Lint>> = phase0_proof_of_concept()
            .into_iter()
            .chain(phase1_packaging_essentials())
            .chain(phase2_correctness())
            .chain(phase3_sections())
            .chain(phase3_changelog_health())
            .chain(phase4_style_source_text())
            .chain(phase5_modernization())
            .chain(phase6_conditional_blocks())
            .chain(phase7_conditional_optimisation())
            .chain(phase7b_extended_conditional())
            .chain(phase7c_multi_branch_refactoring())
            .chain(phase7d_interval_analysis())
            .chain(phase8a_boolean_dnf())
            .chain(phase8b_path_condition_engine())
            .chain(phase8c_macro_propagation())
            .chain(phase9_tree_level_hoisting())
            .chain(phase10_shell_command_modernization())
            .chain(phase11_subpackage_hygiene())
            .chain(phase12_source_url_and_description_style())
            .chain(phase13_shellcheck())
            .chain(phase14_profile_aware())
            .chain(phase15_family_gated())
            .chain(phase17_metadata_cross_tag_consistency())
            .chain(phase18_files_classifier_rules())
            .chain(phase19_scriptlet_install_rules())
            .chain(phase20_policy_registry_driven())
            .chain(phase21_dependency_semantics())
            .chain(phase22_cross_section_dep_policy())
            .chain(phase23_build_install_policy())
            .chain(phase24_conditional_builds_macros())
            .chain(phase25_context_aware_condition_simplification())
            .chain(phase25_path_aware_item_dedup())
            .chain(phase25_prep_setup_patch_modernisation())
            .chain(phase25_macro_modernisation())
            .chain(phase25_files_cleanup())
            .chain(phase25_shell_body_cleanup())
            .chain(phase25_subpackage_style())
            .chain(phase25_rich_dependency_algebra())
            .chain(phase26_repo_aware())
            .collect();
        assert_eq!(all.len(), by_phase.len());
        let all_ids: Vec<&str> = all.iter().map(|l| l.metadata().id).collect();
        let by_phase_ids: Vec<&str> = by_phase.iter().map(|l| l.metadata().id).collect();
        assert_eq!(all_ids, by_phase_ids);
    }
}
