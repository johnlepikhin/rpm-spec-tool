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

mod util;
