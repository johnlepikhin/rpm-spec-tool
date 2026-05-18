//! Cross-rule helpers shared by the lint registry.
//!
//! Organised into topical submodules; this facade re-exports every
//! item that consumers reach via `crate::rules::util::*`.
//!
//! Sub-files:
//! * [`preamble`] — top-level preamble walkers, `Span` helpers, empty-body
//!   predicates, `Patch:` declaration collection.
//! * [`dep_tag`] — [`DepTagKey`] enum classifying dependency-carrying
//!   preamble tags.
//! * [`cond`] — `CondExpr` / `ExprAst` helpers (constant folding,
//!   structural equality, `||` flattening).
//! * [`packages`] — main package + subpackage iteration ([`PackageView`]).
//! * [`deps`] — `DepExpr` / `BoolDep` walkers, canonical text rendering,
//!   structural equality.
//! * [`text`] — `Text` rendering, SPDX atom splitting, path-boundary /
//!   shell-name byte classifiers, fallback path table.
//! * [`macros`] — `declare_missing_tag_lint!` / `declare_missing_section_lint!`
//!   declarative-macro helpers.
//! * [`test_profile`] — `make_test_profile` (cfg-gated to `#[cfg(test)]`).
//!
//! New helpers should join the sub-file matching their topic; the facade
//! exists to keep every consumer path stable as `crate::rules::util::*`.

use rpm_spec_profile::{Family, Profile};

pub(crate) mod cond;
pub(crate) mod dep_tag;
pub(crate) mod deps;
pub(crate) mod macros;
pub(crate) mod packages;
pub(crate) mod preamble;
pub(crate) mod text;

#[cfg(test)]
pub(crate) mod test_profile;

// ---------------------------------------------------------------------
// Re-exports — every previously-public item stays reachable via
// `crate::rules::util::*` so the ~60 consumer rule files keep compiling
// unchanged.
//
// A handful of items (`PatchDecl`, `PackageView`, `contains_macro_ast`,
// `is_spdx_operator`) aren't currently named from any consumer site —
// they're either reached structurally through the return type of
// another helper, or kept on the surface for future rules. `#[allow]`
// keeps the unused-imports warning quiet without losing the exposure.
// ---------------------------------------------------------------------

#[allow(unused_imports)]
pub(crate) use preamble::{
    PatchDecl, collect_declared_patches, collect_top_level_preamble, drop_span, has_top_level_tag,
    is_empty_files_body, is_empty_preamble_body, is_empty_top_body, spec_span,
};

pub(crate) use dep_tag::DepTagKey;

#[allow(unused_imports)]
pub(crate) use cond::{
    cond_expr_resolvably_eq, contains_macro_ast, exprs_equiv, flatten_or,
    is_constant_false_condition, is_constant_true_condition,
};

#[allow(unused_imports)]
pub(crate) use packages::{PackageView, iter_packages, package_name};

pub(crate) use deps::{
    collect_dep_atoms_in_items, collect_top_level_dep_names, dep_atom_text, dep_expr_canonical_eq,
};

#[allow(unused_imports)]
pub(crate) use text::{
    FALLBACK_PATH_TABLE, extract_mkdir_p_target, is_name_byte, is_path_boundary, is_spdx_operator,
    literal_archs, render_text_with_macros, split_spdx_atoms,
};

#[cfg(test)]
pub(crate) use test_profile::make_test_profile;

// `declare_missing_tag_lint!` / `declare_missing_section_lint!` are
// `#[macro_export]`ed at crate root by the `macros` submodule.

// ---------------------------------------------------------------------
// Constants too granular to relocate. The `MACRO_*` names are referenced
// by half-a-dozen rule modules each, none of them clustered enough that
// putting the constants in any one sub-file would feel natural; keeping
// them in the facade preserves their stable import path.
// ---------------------------------------------------------------------

/// Names of well-known macros that lint rules inspect. Kept in one
/// place so a rename in a future RPM release is a single-line change.
pub(crate) const MACRO_SETUP: &str = "setup";
pub(crate) const MACRO_AUTOSETUP: &str = "autosetup";
pub(crate) const MACRO_AUTOPATCH: &str = "autopatch";
pub(crate) const MACRO_PATCH_PREFIX: &str = "patch";

// ---------------------------------------------------------------------
// Profile classifier — small enough that splitting it into its own
// sub-file would dilute the directory; kept inline.
// ---------------------------------------------------------------------

/// `true` when the profile identifies the spec as Fedora or RHEL.
///
/// Used as a positive gate for Fedora/RHEL-specific style rules
/// (RPM125 `source-without-url`, etc). The mirror predicate lives on
/// `rules::bcond_on_non_fedora` and returns `true` for the *complement*
/// (ALT/openSUSE/Mageia/Generic).
///
/// `Family` is `#[non_exhaustive]`, so we deliberately spell out every
/// arm rather than use a wildcard `_`. The compile-time audit is
/// worth the verbosity: when a new variant (`Family::Almalinux`,
/// `Family::Eulerlinux`, …) lands upstream, the missing arm forces a
/// deliberate "is this a RHEL clone? a SUSE clone? something new?"
/// review here instead of silently defaulting one way. Splitting the
/// fallback into `None` and `Some(_)` keeps both intents visible in
/// `cargo expand` / `git blame`.
pub(crate) fn is_fedora_or_rhel(profile: &Profile) -> bool {
    match profile.identity.family {
        Some(Family::Fedora | Family::Rhel) => true,
        // Downstream/internal pipelines: not Fedora/RHEL.
        Some(Family::Alt | Family::Opensuse | Family::Mageia | Family::Generic) => false,
        // No family detected — pre-profile pipelines / `generic` profile.
        None => false,
        // `Family` is `#[non_exhaustive]`; future variants default to
        // `false` (not Fedora/RHEL) until someone makes a deliberate
        // call. Audit this arm whenever a new variant lands upstream.
        Some(_) => false,
    }
}
