//! Reusable analysis primitives for `%files` sections.
//!
//! Phase 18 introduces a single component, [`FilesClassifier`], which
//! the new `%files`-flavoured lint rules (RPM360 onward) share for
//! profile-aware path expansion, directive flattening, and shape
//! detection (devel/locale/systemd/...).
//!
//! Rules do not call into the AST's macro registry directly any more —
//! they ask the classifier for resolved paths and flag summaries.

pub(crate) mod classifier;
pub(crate) mod walk;

// Selectively re-export the types call sites name explicitly. The
// remainder (`EntryClassification`, `DirectiveSummary`, `AttrSummary`)
// are reached through inference on `classify()`'s return type and the
// field projections off it; re-exporting them at the module root
// would only add unused-import noise.
pub(crate) use classifier::{ConfigKind, FilesClassifier, KindHints};
pub(crate) use walk::{
    for_each_files_entry, for_each_files_entry_with_subpkg, for_each_files_section,
    neighbour_is_comment, pkg_name_for,
};
