//! Built-in lint rules. Add new ones here and register them in
//! [`crate::registry::builtin_lints`].

pub mod empty_description;
pub mod missing_changelog;

pub mod deprecated_clean_section;
pub mod missing_tag;
pub mod multiple_changelog;
pub mod obsolete_tag;

mod util;
