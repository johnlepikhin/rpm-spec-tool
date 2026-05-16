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

mod util;
