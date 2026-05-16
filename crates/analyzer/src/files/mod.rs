//! Reusable analysis primitives for `%files` sections.
//!
//! Phase 18 introduces a single component, [`FilesClassifier`], which
//! the new `%files`-flavoured lint rules (RPM360 onward) share for
//! profile-aware path expansion, directive flattening, and shape
//! detection (devel/locale/systemd/...).
//!
//! Rules do not call into the AST's macro registry directly any more —
//! they ask the classifier for resolved paths and flag summaries.

pub mod classifier;
pub mod walk;

pub use classifier::{
    AttrSummary, ConfigKind, DirectiveSummary, EntryClassification, FilesClassifier, KindHints,
};
pub use walk::{
    for_each_files_entry, for_each_files_entry_with_subpkg, for_each_files_section,
    neighbour_is_comment, resolve_subpkg_name,
};
