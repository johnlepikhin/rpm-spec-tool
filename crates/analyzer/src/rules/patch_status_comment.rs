//! RPM307 `patch-status-comment-missing` — openSUSE-only: every
//! `PatchN:` tag should have a neighbouring comment with a status /
//! origin marker.
//!
//! openSUSE's packaging convention asks reviewers to be able to answer
//! "is this patch upstream, distro-specific, or rebase-pending?" from
//! the spec alone. The convention is enforced by `spec-cleaner` and
//! documented on the wiki — markers look like:
//!
//! ```text
//! # PATCH-FIX-UPSTREAM 0001-foo.patch alice@suse.com -- already merged in 1.2
//! # PATCH-FIX-OPENSUSE crash-on-empty-input.patch bob@suse.com -- distro-only
//! # PATCH-FEATURE-UPSTREAM enable-tls.patch carol@suse.com -- submitted PR #123
//! # PATCH-NEEDS-REBASE port-to-glib-2.74.patch dave@suse.com -- pending
//! Patch0:   0001-foo.patch
//! ```
//!
//! The rule fires only on profiles with `family = Opensuse`. We accept
//! a marker on any line of the contiguous comment block immediately
//! above the `PatchN:` tag — `spec-cleaner` lays them out that way,
//! but a single line is enough. A marker is identified by the *first
//! whitespace-separated token* of the comment text (case-insensitive)
//! starting with one of the recognised prefixes; this avoids
//! false-silencing when a description line happens to mention
//! "PATCH-FIX-…" in prose.

use rpm_spec::ast::{Conditional, Span, SpecFile, SpecItem, Tag};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;
use rpm_spec_profile::{Family, Profile};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM307",
    name: "patch-status-comment-missing",
    description: "openSUSE: every `Patch:` tag should be preceded by a comment carrying a \
                  status marker (`PATCH-FIX-UPSTREAM`, `PATCH-FIX-OPENSUSE`, \
                  `PATCH-FEATURE-*`, `PATCH-NEEDS-*`, ...). Without it reviewers can't tell \
                  whether the patch is upstream-bound.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct PatchStatusCommentMissing {
    diagnostics: Vec<Diagnostic>,
    enabled: bool,
}

impl PatchStatusCommentMissing {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Recognised marker prefixes (uppercase, anchored at the start of the
/// first comment token). Order doesn't matter — `starts_with` is used.
/// The shorter prefixes (`PATCH-FIX-`, `PATCH-FEATURE-`, `PATCH-NEEDS-`)
/// subsume their specific siblings, so the list is intentionally
/// minimal: an entry is added only when it's not already covered.
const STATUS_MARKER_PREFIXES: &[&str] = &[
    "PATCH-FIX-",
    "PATCH-FEATURE-",
    "PATCH-NEEDS-",
    "PATCH-PORT-",
    "PATCH-ENABLE-",
    "PATCH-DISABLE-",
    "PATCH-MISSING-TAG",
];

impl<'ast> Visit<'ast> for PatchStatusCommentMissing {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if !self.enabled {
            return;
        }
        scan_items(&spec.items, &mut self.diagnostics);
    }
}

/// Walk a flat `&[SpecItem<Span>]`, descending into top-level
/// `Conditional` arms so `Patch:` tags hidden behind `%if 0%{?with_x}`
/// are still examined. Neighbour-comment lookup uses the *enclosing*
/// arm: a `%if` arm with `# marker` followed by `Patch:` is fine; a
/// marker outside the arm doesn't count.
fn scan_items(items: &[SpecItem<Span>], out: &mut Vec<Diagnostic>) {
    for (i, item) in items.iter().enumerate() {
        match item {
            SpecItem::Preamble(p) => {
                if !matches!(p.tag, Tag::Patch(_)) {
                    continue;
                }
                if preceding_comments_have_marker(items, i) {
                    continue;
                }
                out.push(Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "openSUSE: this `Patch:` tag has no status-marker comment \
                     (`PATCH-FIX-UPSTREAM` / `PATCH-FIX-OPENSUSE` / `PATCH-NEEDS-REBASE` / ...)",
                    p.data,
                ));
            }
            SpecItem::Conditional(c) => scan_conditional(c, out),
            _ => {}
        }
    }
}

fn scan_conditional(cond: &Conditional<Span, SpecItem<Span>>, out: &mut Vec<Diagnostic>) {
    for branch in &cond.branches {
        scan_items(&branch.body, out);
    }
    if let Some(els) = &cond.otherwise {
        scan_items(els, out);
    }
}

/// Walk backwards from `idx-1` through contiguous comments and report
/// whether any of them carries a known status marker. A single blank
/// line is tolerated between the block and the tag; anything else
/// (another preamble item, a section, …) terminates the walk.
fn preceding_comments_have_marker(items: &[SpecItem<Span>], idx: usize) -> bool {
    for item in items[..idx].iter().rev() {
        match item {
            SpecItem::Comment(c) => {
                if comment_has_marker(&c.text) {
                    return true;
                }
            }
            SpecItem::Blank => continue,
            _ => return false,
        }
    }
    false
}

/// `true` when the first whitespace-separated token of `text`
/// case-insensitively starts with one of the recognised marker
/// prefixes. Anchoring at the first token avoids a false negative on
/// `# This refers to the PATCH-FIX- convention` silencing the lint on
/// an unrelated patch nearby.
fn comment_has_marker(text: &rpm_spec::ast::Text) -> bool {
    let Some(lit) = text.literal_str() else {
        return false;
    };
    let Some(first_token) = lit.split_whitespace().next() else {
        return false;
    };
    let first_upper = first_token.to_ascii_uppercase();
    STATUS_MARKER_PREFIXES
        .iter()
        .any(|p| first_upper.starts_with(p))
}

impl Lint for PatchStatusCommentMissing {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        matches!(profile.identity.family, Some(Family::Opensuse))
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.enabled = matches!(profile.identity.family, Some(Family::Opensuse));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn opensuse() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Opensuse);
        p
    }

    fn fedora() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Fedora);
        p
    }

    fn run_with(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = PatchStatusCommentMissing::new();
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_patch_without_status_comment() {
        let src = "Name: x\nPatch0: foo.patch\n";
        let diags = run_with(src, &opensuse());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM307");
    }

    #[test]
    fn silent_with_upstream_marker() {
        let src =
            "Name: x\n# PATCH-FIX-UPSTREAM foo.patch alice -- merged in 1.2\nPatch0: foo.patch\n";
        assert!(run_with(src, &opensuse()).is_empty());
    }

    #[test]
    fn silent_with_opensuse_marker() {
        let src =
            "Name: x\n# PATCH-FIX-OPENSUSE crash.patch bob -- distro only\nPatch1: crash.patch\n";
        assert!(run_with(src, &opensuse()).is_empty());
    }

    #[test]
    fn silent_with_needs_rebase_marker() {
        let src = "Name: x\n# PATCH-NEEDS-REBASE port.patch carol\nPatch0: port.patch\n";
        assert!(run_with(src, &opensuse()).is_empty());
    }

    #[test]
    fn silent_when_marker_is_lowercase() {
        let src = "Name: x\n# patch-fix-upstream foo.patch\nPatch0: foo.patch\n";
        assert!(run_with(src, &opensuse()).is_empty());
    }

    #[test]
    fn flags_when_neighbour_comment_lacks_marker() {
        let src = "Name: x\n# arbitrary comment, no marker\nPatch0: foo.patch\n";
        assert_eq!(run_with(src, &opensuse()).len(), 1);
    }

    #[test]
    fn silent_with_blank_between_comment_and_patch() {
        let src = "Name: x\n# PATCH-FIX-UPSTREAM foo.patch\n\nPatch0: foo.patch\n";
        assert!(run_with(src, &opensuse()).is_empty());
    }

    #[test]
    fn flags_only_unmarked_patch_in_mixed_list() {
        let src = "Name: x\n# PATCH-FIX-UPSTREAM a.patch\nPatch0: a.patch\nPatch1: b.patch\n";
        let diags = run_with(src, &opensuse());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_on_fedora_profile() {
        let src = "Name: x\nPatch0: foo.patch\n";
        assert!(run_with(src, &fedora()).is_empty());
    }

    #[test]
    fn silent_on_generic_profile() {
        let src = "Name: x\nPatch0: foo.patch\n";
        assert!(run_with(src, &Profile::default()).is_empty());
    }

    #[test]
    fn marker_inside_a_longer_comment_block_counts() {
        // Marker first, description following — common spec-cleaner shape.
        let src = "Name: x\n# PATCH-FIX-UPSTREAM foo.patch alice\n# Detailed description of the fix, multi-line.\nPatch0: foo.patch\n";
        assert!(run_with(src, &opensuse()).is_empty());
    }

    #[test]
    fn marker_below_description_counts() {
        // Description first, marker last (right above the Patch tag).
        let src = "Name: x\n# Detailed description of the fix.\n# PATCH-FIX-UPSTREAM foo.patch alice\nPatch0: foo.patch\n";
        assert!(run_with(src, &opensuse()).is_empty());
    }

    #[test]
    fn flags_description_mentioning_marker_in_prose() {
        // Prose that mentions `PATCH-FIX-` mid-line must NOT silence
        // the lint — the marker has to be the first token of the comment.
        let src =
            "Name: x\n# Discussion about the PATCH-FIX-UPSTREAM convention\nPatch0: foo.patch\n";
        assert_eq!(run_with(src, &opensuse()).len(), 1);
    }

    #[test]
    fn flags_patch_inside_conditional_without_marker() {
        // `%if` body still counted.
        let src = "Name: x\n%if 0%{?suse_version}\nPatch99: foo.patch\n%endif\n";
        assert_eq!(run_with(src, &opensuse()).len(), 1);
    }

    #[test]
    fn silent_for_patch_inside_conditional_with_marker() {
        let src = "Name: x\n%if 0%{?suse_version}\n# PATCH-FIX-OPENSUSE foo.patch\nPatch99: foo.patch\n%endif\n";
        assert!(run_with(src, &opensuse()).is_empty());
    }
}
