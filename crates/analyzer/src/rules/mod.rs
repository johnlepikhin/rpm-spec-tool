//! Built-in lint rules. Add new ones here and register them in
//! [`crate::registry::builtin_lints`].

pub mod empty_description;
pub mod missing_changelog;

pub mod deprecated_clean_section;
pub mod missing_tag;
pub mod multiple_changelog;
pub mod obsolete_tag;

// Phase 2 — Correctness rules.
pub mod macro_redefinition;
pub mod obsolete_without_provides;
pub mod requires_equal_version;
pub mod self_conflict;
pub mod self_obsoletion;
pub mod useless_explicit_provides;

// Phase 3 — Sections and changelog.
pub mod changelog_health;
pub mod duplicate_buildscript;
pub mod missing_section;

// Phase 4 — Style / source-text.
pub mod description_health;
pub mod hardcoded_paths;
pub mod macro_in_hash_comment;
pub mod shell_vars;
pub mod summary_style;
pub mod tab_indent;
pub mod trailing_whitespace;

// Phase 5 — Modernization.
pub mod deprecated_commands;
pub mod patch_tracking;
pub mod setup_flags;

// Phase 6 — Conditional-block lints.
pub mod conditional_merge;
pub mod conditional_simplify;
pub mod conditional_structure;

// Phase 7 — Conditional optimisation.
pub mod conditional_optimize;

// Phase 7b — extended conditional lints.
pub mod conditional_factoring;
pub mod conditional_idioms;

// Phase 7c — multi-branch refactoring.
pub mod conditional_hoist;

// Phase 7d — interval analysis.
pub mod conditional_intervals;

// Phase 8a — boolean DNF normalisation.
pub mod boolean_dnf;

// Phase 8b — path-condition engine.
pub mod always_true_branch;
pub mod dead_elif;
pub mod exhaustive_chain;
pub(crate) mod path_cond;
pub mod unreachable_branch;

// Phase 8c — macro value propagation.
pub mod macro_propagation;

// Phase 9 — tree-level hoisting.
pub mod leaf_hoist;

// Phase 10 — shell-command modernization.
pub mod shell_modernization;

// Phase 11 — subpackage hygiene.
pub mod subpackage_hygiene;

// Phase 12 — source URL + description style.
pub mod source_style;

// Phase 13 — shellcheck integration.
pub mod shellcheck;

// Phase 14 — profile-aware lints.
pub mod invalid_license;
pub mod non_standard_group;

// Phase 15 — family-gated rules (emit/no-emit polarity via
// Lint::applies_to_profile).
pub mod bcond_on_non_fedora;
pub mod group_tag_required_on_suse;
pub mod legacy_license_syntax;

mod util;
