//! RPM471 `patch-sequence-to-autopatch` — flag `%prep` bodies that
//! apply every declared patch via a sequence of `%patch -P N -pK`
//! calls with a single consistent strip flag.
//!
//! When the sequence covers exactly the set of declared `PatchN:`
//! tags and every call uses the same `-p` value, the user can replace
//! the whole block with a single `%autopatch -pK`.

use std::collections::BTreeSet;

use rpm_spec::ast::{Span, SpecFile, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::prep_model::find_prep_body_with_span;
use crate::rules::util::{
    MACRO_AUTOPATCH, MACRO_AUTOSETUP, MACRO_PATCH_PREFIX, collect_declared_patches,
};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM471",
    name: "patch-sequence-to-autopatch",
    description: "Every declared patch is applied via `%patch -P N -pK` with the same strip \
                  value — replace the sequence with a single `%autopatch -pK`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Every declared patch is applied via `%patch -P N -pK` with the same strip value — replace the sequence with a single `%autopatch -pK`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct PatchSequenceToAutopatch {
    diagnostics: Vec<Diagnostic>,
}

impl PatchSequenceToAutopatch {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for PatchSequenceToAutopatch {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let declared = collect_declared_patches(spec);
        if declared.len() < 2 {
            return;
        }
        let declared_nums: BTreeSet<u32> = declared.iter().map(|p| p.number).collect();
        let Some((body, prep_span)) = find_prep_body_with_span(spec) else {
            return;
        };
        let mut applied: BTreeSet<u32> = BTreeSet::new();
        let mut strip: Option<Option<u32>> = None; // outer Option: "seen any"; inner: strip value
        let mut shape_ok = true;
        for line in &body.lines {
            for (i, seg) in line.segments.iter().enumerate() {
                let TextSegment::Macro(m) = seg else {
                    continue;
                };
                let name = m.name.as_str();
                if name == MACRO_AUTOPATCH || name == MACRO_AUTOSETUP {
                    // Already using the macro the rule suggests.
                    return;
                }
                let Some(rest) = name.strip_prefix(MACRO_PATCH_PREFIX) else {
                    continue;
                };
                if !rest.is_empty() {
                    // Legacy `%patchN` form — not the canonical
                    // `%patch -P N -pK` shape we suggest folding.
                    shape_ok = false;
                    continue;
                }
                // `%patch ...` — parse trailing tokens for -P N and -pK.
                let trailing = &line.segments[i + 1..];
                let parsed = parse_patch_args(trailing);
                let Some(parsed) = parsed else {
                    shape_ok = false;
                    continue;
                };
                applied.insert(parsed.num);
                match strip {
                    None => strip = Some(parsed.strip),
                    Some(existing) if existing == parsed.strip => {}
                    Some(_) => {
                        // Mixed `-p` values — can't collapse.
                        return;
                    }
                }
            }
        }
        if !shape_ok {
            return;
        }
        if applied != declared_nums {
            return;
        }
        // Need at least one strip value seen; consistent by construction.
        let Some(strip_value) = strip else {
            return;
        };
        let strip_hint = match strip_value {
            Some(n) => format!(" -p{n}"),
            None => String::new(),
        };
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`%prep` applies every declared patch via a `%patch -P N{strip_hint}` \
                     sequence — replace with a single `%autopatch{strip_hint}`"
                ),
                prep_span,
            )
            .with_suggestion(Suggestion::new(
                "use `%autopatch` to apply every declared patch at once",
                Vec::new(),
                Applicability::MaybeIncorrect,
            )),
        );
    }
}

#[derive(Debug)]
struct PatchCall {
    num: u32,
    strip: Option<u32>,
}

/// Parse trailing literal text after a `%patch` macro for `-P N` and
/// `-pK` flags. Returns `None` if `N` isn't a literal integer, or if
/// any non-literal segment is encountered.
fn parse_patch_args(segs: &[TextSegment]) -> Option<PatchCall> {
    let mut text = String::new();
    for seg in segs {
        match seg {
            TextSegment::Literal(s) => text.push_str(s),
            TextSegment::Macro(_) => return None,
            _ => return None,
        }
    }
    let mut tokens = text.split_ascii_whitespace().peekable();
    let mut num: Option<u32> = None;
    let mut strip: Option<u32> = None;
    while let Some(t) = tokens.next() {
        if t == "-P" {
            num = tokens.next().and_then(|s| s.parse::<u32>().ok());
        } else if let Some(rest) = t.strip_prefix("-P") {
            num = rest.parse::<u32>().ok();
        } else if t == "-p" {
            strip = tokens.next().and_then(|s| s.parse::<u32>().ok());
        } else if let Some(rest) = t.strip_prefix("-p") {
            strip = rest.parse::<u32>().ok();
        }
    }
    num.map(|n| PatchCall { num: n, strip })
}

impl Lint for PatchSequenceToAutopatch {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<PatchSequenceToAutopatch>(src)
    }

    #[test]
    fn flags_full_sequence_with_consistent_strip() {
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
%prep\n\
%setup -q\n\
%patch -P 0 -p1\n\
%patch -P 1 -p1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM471");
        assert!(diags[0].message.contains("-p1"));
    }

    #[test]
    fn silent_when_strip_inconsistent() {
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
%prep\n\
%patch -P 0 -p0\n\
%patch -P 1 -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_sequence_incomplete() {
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
%prep\n\
%patch -P 0 -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_autopatch_already_used() {
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
%prep\n\
%autopatch -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_legacy_patch_form() {
        // `%patch1` legacy form falls outside the canonical `%patch -P N` shape.
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
%prep\n\
%patch0 -p1\n\
%patch1 -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_only_one_patch_declared() {
        // RPM471 is a "collapse a sequence" rule; one declared patch
        // isn't worth a sequence.
        let src = "Name: x\nPatch0: a.patch\n%prep\n%patch -P 0 -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_strip_zero() {
        // No `-p` flag → strip 0. Three patches, all with no -p.
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
%prep\n\
%patch -P 0\n\
%patch -P 1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        // No strip suffix in message.
        assert!(!diags[0].message.contains("-p"));
    }
}
