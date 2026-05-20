//! Built-in lint rules. Add new ones here and register them in
//! [`crate::registry::builtin_lints`].

// NOTE: `EXPAND_DEPTH = 8` is the conventional macro-expansion cap used by
// branch_coverage, files/classifier, macro_composition_to_specific. The
// RPM-REPO-* rules use `MACRO_EXPAND_DEPTH` for self-documentation. Both
// equal 8; consolidate into a single `rpm_spec_profile` const if a third
// naming emerges.

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
pub(crate) mod prep_model;
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

// Phase 17 — metadata / cross-tag consistency.
pub mod arch_policy;
pub mod autoreqprov_comment;
pub mod buildarch_reparse;
pub mod duplicate_singleton_tag;
pub mod nvre_format;
pub mod source_patch_list;
pub mod source_version_consistency;
pub mod spec_filename;
pub mod subpackage_name_collision;

// Phase 18 — `%files` rules built on `FilesClassifier`.
pub mod files_attr;
pub mod files_config;
pub mod files_debuginfo;
pub mod files_devel;
pub mod files_duplicate;
pub mod files_license;
pub mod files_locale;
pub mod files_standard;
pub mod files_volatile;

// Phase 19 — scriptlet/install rules built on CommandUseIndex + shell walkers.
pub mod install_boundaries;
pub mod install_chown;
pub mod install_make;
pub mod scriptlet_commands;
pub mod scriptlet_health;

// Phase 20 — PolicyRegistry-driven systemd/tmpfiles/users rules.
pub mod ldconfig_style;
pub mod release_disttag;
pub mod systemd_units;
pub mod tmpfiles_create;
pub mod users_groups;

// Phase 21 — dependency semantics.
pub mod dep_features;
pub mod dep_health;

// Phase 22 — cross-section dep policy (CommandUseIndex × FilesClassifier × PolicyRegistry).
pub mod build_tool_brs;
pub mod patch_status_comment;
pub mod pkgconfig_br;
pub mod scriptlet_deps;

// Phase 23 — build/install policy.
pub mod buildsystem_macros;
pub mod disabled_check;
pub mod network_in_build;
pub mod optflags;
pub mod parallel_make;
pub mod werror;

// Phase 24 — conditional builds / macros.
pub mod bcond_modern;
pub mod bcond_usage;
pub mod include_notice;
pub mod metadata_shell_macro;

// Phase 25 — context-aware condition simplification (RPM430+).
pub mod condition_common_disjunct_factor;
pub mod condition_common_factor;
pub mod context_redundant_part;
pub mod elif_history_simplify;
pub mod equality_chain_to_range;
pub mod negated_comparison_simplify;
pub mod unnecessary_condition_parens;

// Phase 25 — adjacency / merge / arch (RPM439, RPM442, RPM444+).
pub mod adjacent_mutex_ifs;
pub mod arch_complement_shorter;
pub mod arch_condition_domain_simplify;
pub mod arch_subset_under_parent;
pub mod canonical_major_section_order;
pub mod preamble_tag_clustering;
pub mod repeated_comment_before_guards;
pub mod same_body_arch_blocks;
pub mod target_cpu_equality;

// Phase 25 — %files cleanup (RPM510+).
pub mod adjacent_doc_lines;
pub mod adjacent_license_lines;
pub mod files_directory_subsumes_child;
pub mod files_glob_subsumes_explicit;
pub mod files_section_sort_blocks;
pub mod redundant_default_defattr;
pub mod repeated_files_prefix;

// Phase 25 — shell-body cleanup (RPM530+).
pub mod combine_install_dirs;
pub mod cp_chmod_to_install_m;
pub mod duplicate_shell_block;
pub mod mkdir_install_to_install_d;
pub mod near_duplicate_shell_block;
pub mod redundant_mkdir_before_install_d;
pub mod repeated_rm_f;
pub(crate) mod shell_walk;

// Phase 25 — path-aware item dedup / hoisting (RPM450+).
pub mod branch_item_subset;
pub mod complementary_guards_same_item;
pub mod full_domain_conditional_item;
pub mod guarded_item_already_unconditional;
pub mod guarded_item_dominated;
pub mod repeated_ifelse_value_extraction;
pub mod same_body_different_conditions;
pub mod same_guard_clustering;

// Phase 25 — prep / setup / patch modernisation (RPM470+).
pub mod long_patch_list_to_patchlist;
pub mod manual_extra_source_unpack;
pub mod manual_patch_command;
pub mod manual_tar_cd_to_setup;
pub mod patch_sequence_to_autopatch;
pub mod redundant_cd_after_setup;
pub mod redundant_setup_default_name;
pub mod setup_autopatch_to_autosetup;

// Phase 25 — macro modernisation (RPM436, RPM437, RPM438, RPM490+).
pub mod bcond_negation_canonical;
pub mod commented_out_spec_code;
pub mod description_equals_summary;
pub mod duplicate_macro_bodies;
pub mod empty_optional_macro_arm;
pub mod excessive_section_separators;
pub mod long_literal_prefix;
pub mod macro_alias_of_builtin;
pub mod macro_called_same_arg;
pub mod macro_composition_to_specific;
pub mod macro_name_shadows_bcond_helper;
pub mod mixed_subpackage_reference_style;
pub mod no_op_conditional_macro;
pub mod optional_macro_boolean_shortening;
pub mod redundant_subpackage_version_release;
pub mod repeated_subpackage_boilerplate;
pub mod single_use_private_macro;
pub mod stale_disabled_source_or_patch;
pub mod subpackage_description_copy;
pub mod unused_macro_parameter;

// Phase 25 — subpackage style (RPM550+).
pub mod prefer_relative_subpackage_name;

// Phase 25 — rich dependency algebra.
pub mod dependency_constraint_subsumption;
pub mod guarded_dependency_subsumption;
pub mod richdep_absorption;
pub mod richdep_common_factor;
pub mod richdep_idempotent;
pub mod richdep_nested_flatten;
pub mod richdep_same_then_else;
pub mod richdep_singleton;

// Repository-aware lints (RPM-REPO-*). Consume the `RepoUniverse`
// provided by the CLI / analyzer session and emit findings only when
// at least one configured repo's metadata is cached. See
// `repo/mod.rs` for the rule reference table.
pub mod repo;

pub(crate) mod util;

#[cfg(test)]
mod test_support;
