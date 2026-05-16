//! RPM404 `macro-shell-expansion-in-metadata` — a metadata tag
//! (`Version`, `Release`, `Source*`, `URL`, `Epoch`, `Summary`,
//! `License`, etc.) carries a `%(shell)` substitution or a
//! `%{lua:...}` block.
//!
//! Shell-expansion and Lua-substitution in metadata produce a
//! different package every time the spec is built — the SRPM
//! filename, NVR, and changelog become time-dependent or
//! environment-dependent. RPM does support `%(...)` everywhere, but
//! using it in identity tags breaks reproducible builds, makes
//! cross-distro patch propagation harder, and routinely surprises
//! reviewers who diff two SRPMs of "the same" package.
//!
//! The rule fires on a curated set of *identity* tags only. Free-form
//! tags like `Group` are ignored — there the substitution is rare and
//! usually deliberate. (`%description` is a section, not a preamble
//! tag, so it is naturally out of scope.)

use rpm_spec::ast::{
    MacroKind, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, Tag, TagValue,
    Text, TextSegment,
};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM404",
    name: "macro-shell-expansion-in-metadata",
    description: "An identity tag (`Version`, `Release`, `Source*`, `URL`, …) carries `%(shell)` \
                  or `%{lua:...}`. The expansion is evaluated at build time, so the resulting \
                  NVR / SRPM filename / changelog changes between builds — reproducibility \
                  breaks.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Lint state for RPM404.
#[derive(Debug, Default)]
pub struct MacroShellExpansionInMetadata {
    diagnostics: Vec<Diagnostic>,
}

impl MacroShellExpansionInMetadata {
    /// Construct an empty lint instance with no diagnostics buffered.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MacroShellExpansionInMetadata {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        self.walk_top_items(&spec.items);
    }
}

impl MacroShellExpansionInMetadata {
    fn walk_top_items(&mut self, items: &[SpecItem<Span>]) {
        for item in items {
            match item {
                SpecItem::Preamble(p) => self.check_item(p),
                SpecItem::Section(boxed) => {
                    if let Section::Package { content, .. } = boxed.as_ref() {
                        self.walk_preamble_content(content);
                    }
                }
                SpecItem::Conditional(c) => {
                    for branch in &c.branches {
                        self.walk_top_items(&branch.body);
                    }
                    if let Some(els) = &c.otherwise {
                        self.walk_top_items(els);
                    }
                }
                _ => {}
            }
        }
    }

    fn walk_preamble_content(&mut self, items: &[PreambleContent<Span>]) {
        for item in items {
            match item {
                PreambleContent::Item(p) => self.check_item(p),
                PreambleContent::Conditional(c) => {
                    for branch in &c.branches {
                        self.walk_preamble_content(&branch.body);
                    }
                    if let Some(els) = &c.otherwise {
                        self.walk_preamble_content(els);
                    }
                }
                _ => {}
            }
        }
    }

    fn check_item(&mut self, p: &PreambleItem<Span>) {
        let Some(label) = identity_tag_label(&p.tag) else {
            return;
        };
        let Some(kind) = volatile_macro_in_value(&p.value) else {
            return;
        };
        let kind_label = kind.label();
        self.diagnostics.push(Diagnostic::new(
            &METADATA,
            Severity::Warn,
            format!(
                "`{label}` contains {kind_label}; the expansion is evaluated at build time and \
                 breaks reproducibility — move the computation to a `%global` near the top of \
                 the spec and reference its result here"
            ),
            p.data,
        ));
    }
}

/// Identity tags whose value should be a static identity. Free-form
/// tags (Group, Vendor, BuildArch, ExcludeArch, …) are outside this
/// set — `%(...)` there is rare and usually intentional.
fn identity_tag_label(tag: &Tag) -> Option<String> {
    match tag {
        Tag::Name => Some("Name".into()),
        Tag::Version => Some("Version".into()),
        Tag::Release => Some("Release".into()),
        Tag::Epoch => Some("Epoch".into()),
        Tag::Summary => Some("Summary".into()),
        Tag::License => Some("License".into()),
        Tag::URL => Some("URL".into()),
        Tag::Source(n) => Some(match n {
            Some(n) => format!("Source{n}"),
            None => "Source".into(),
        }),
        Tag::Patch(n) => Some(match n {
            Some(n) => format!("Patch{n}"),
            None => "Patch".into(),
        }),
        _ => None,
    }
}

/// Classification of the volatile macro flavour we report on. Kept
/// separate from presentation so the message text lives in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolatileKind {
    Shell,
    Lua,
}

impl VolatileKind {
    fn label(self) -> &'static str {
        match self {
            Self::Shell => "a `%(...)` shell expansion",
            Self::Lua => "a `%{lua:...}` block",
        }
    }
}

/// Inspect a tag value and report the kind of volatile macro found
/// (`%(...)` shell or `%{lua:...}`), if any.
fn volatile_macro_in_value(v: &TagValue) -> Option<VolatileKind> {
    match v {
        TagValue::Text(t) => volatile_macro_in_text(t),
        TagValue::ArchList(items) => items.iter().find_map(volatile_macro_in_text),
        _ => None,
    }
}

/// Walk a [`Text`] looking for a `%(...)` or `%{lua:...}` macro
/// reference. Recurses into parametric macro arguments and the
/// `%{?cond:VALUE}` body so that nested cases like
/// `Version: %{?dist:%(date)}` are caught as well.
fn volatile_macro_in_text(t: &Text) -> Option<VolatileKind> {
    t.segments.iter().find_map(|seg| {
        let TextSegment::Macro(m) = seg else {
            return None;
        };
        let direct = match m.kind {
            MacroKind::Shell => Some(VolatileKind::Shell),
            MacroKind::Lua => Some(VolatileKind::Lua),
            _ => None,
        };
        direct
            .or_else(|| m.args.iter().find_map(volatile_macro_in_text))
            .or_else(|| m.with_value.as_ref().and_then(volatile_macro_in_text))
    })
}

impl Lint for MacroShellExpansionInMetadata {
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
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = MacroShellExpansionInMetadata::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_shell_in_version() {
        let src = "Name: x\nVersion: %(date +%Y%m%d)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM404");
        assert!(diags[0].message.contains("Version"));
        assert!(diags[0].message.contains("%(..."));
    }

    #[test]
    fn flags_lua_in_release() {
        let src = "Name: x\nRelease: %{lua:print(os.time())}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Release"));
        assert!(diags[0].message.contains("lua"));
    }

    #[test]
    fn flags_shell_in_source0() {
        let src = "Name: x\nSource0: https://example.com/%(date).tar.gz\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("Source0"),
            "expected indexed label, got: {}",
            diags[0].message
        );
    }

    #[test]
    fn flags_shell_in_url() {
        let src = "Name: x\nURL: %(curl -s https://example.com/redirect)\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_normal_metadata() {
        let src = "Name: x\nVersion: 1.0\nRelease: 1%{?dist}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_shell_in_description() {
        // `%description` body is shell-bearing and free-form; out of
        // scope for RPM404.
        let src = "Name: x\nVersion: 1\n%description\nbuilt on %(date)\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_normal_macro_in_release() {
        let src = "Name: x\nRelease: 1%{?dist}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_in_subpackage_version() {
        let src = "Name: x\nVersion: 1\n\
%package devel\n\
Version: %(date +%Y%m%d)\n\
%description devel\nbody\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_inside_conditional_branch() {
        let src = "Name: x\n%if 0%{?fedora}\nVersion: %(date +%Y)\n%endif\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_multiple_distinct_tags() {
        let src = "Name: x\nVersion: %(date +%Y)\nRelease: %{lua:print(1)}\n";
        assert_eq!(run(src).len(), 2);
    }

    #[test]
    fn flags_shell_in_patch0() {
        let src = "Name: x\nVersion: 1\nPatch0: fix-%(date).patch\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("Patch0"),
            "expected indexed label, got: {}",
            diags[0].message
        );
    }

    #[test]
    fn flags_lua_in_summary() {
        let src = "Name: x\nSummary: %{lua:print('hi')}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("Summary"));
        assert!(diags[0].message.contains("lua"));
    }

    #[test]
    fn flags_shell_in_license() {
        let src = "Name: x\nLicense: %(echo MIT)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("License"));
    }

    #[test]
    fn flags_shell_in_epoch() {
        // Non-numeric Epoch value falls back to TagValue::Text in the
        // parser, which is exactly when this lint can still apply.
        let src = "Name: x\nEpoch: %(echo 1)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("Epoch"));
    }

    #[test]
    fn flags_shell_in_name() {
        let src = "Name: %(echo foo)\nVersion: 1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("Name"));
    }

    #[test]
    fn silent_for_shell_in_group() {
        // Group is intentionally outside the identity-tag set.
        let src = "Name: x\nVersion: 1\nGroup: %(echo Applications/Text)\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_shell_nested_in_with_value() {
        // `%{?dist:%(date)}` — the `%(...)` lives inside the
        // conditional macro's `with_value`, so the lint must recurse.
        let src = "Name: x\nVersion: %{?dist:%(date)}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("Version"));
        assert!(diags[0].message.contains("%(..."));
    }
}
