//! Dependency-hygiene rules: RPM320, RPM321, RPM322, RPM323.
//!
//! All four operate on the dependency atoms reachable from a
//! `PackageView` and share the same atom-extraction primitives. They
//! live in one file because the per-rule logic is small and the file
//! boundary would be pure ceremony.
//!
//! - **RPM320 `duplicate-dependency-atom`** — the same atom appears
//!   more than once inside one tag's value list (`Requires: foo,
//!   foo`). Two entries for the same package are merge artifacts;
//!   rpm keeps one but the duplicate is noise. The dedup key is the
//!   atom's `(name, arch_qualifier)` pair so that
//!   `Requires: pkgconfig(openssl), pkgconfig(zlib)` (two *distinct*
//!   capabilities that both parse with `name = "pkgconfig"`) is not
//!   falsely flagged.
//! - **RPM321 `weak-dep-duplicates-strong-dep`** — `Requires: X` plus
//!   `Recommends: X` (or `Suggests:`) is redundant: weak deps only
//!   matter when the strong one isn't already pulling the package in.
//!   Same `(name, arch)` key as RPM320.
//! - **RPM322 `self-weak-dependency`** — `Recommends:`/`Suggests:`/
//!   `Supplements:`/`Enhances:` naming the package itself. Almost
//!   always copy-paste from another spec — RPM treats it as a no-op
//!   but downstream review tools complain.
//! - **RPM323 `runtime-requires-looks-like-build-requires`** —
//!   `Requires:` mentions a build-only artifact (`gcc`, `cmake`,
//!   `*-devel`, `pkgconfig(...)`). Build tools belong in
//!   `BuildRequires:`; the runtime listing pulls them in for every
//!   user of the package.
//!
//! Diagnostic anchors point at the offending `PreambleItem` rather
//! than the surrounding package header so editor jump-to-finding
//! lands on the right line.

use std::collections::BTreeSet;

use rpm_spec::ast::{
    BoolDep, DepAtom, DepExpr, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem,
    Tag, TagValue,
};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::iter_packages;
use crate::visit::Visit;

/// Normalised key used to dedup or look up an atom across tags.
/// Captures `(name, arch_qualifier)` so `pkgconfig(openssl)` and
/// `pkgconfig(zlib)` — both parsed with `name = "pkgconfig"` but
/// `arch = Some("openssl"|"zlib")` — are treated as distinct.
type AtomKey = (String, Option<String>);

/// Build an [`AtomKey`] when both the atom name and any present arch
/// qualifier are pure literals. Returns `None` when either contains
/// macros we can't resolve at lint time.
fn atom_key(atom: &DepAtom) -> Option<AtomKey> {
    let name = atom.name.literal_str()?.trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let arch = match atom.arch.as_ref() {
        Some(text) => Some(text.literal_str()?.trim().to_owned()),
        None => None,
    };
    Some((name, arch))
}

/// Render an [`AtomKey`] back to the spec surface (`name` or
/// `name(arch)`) for diagnostic messages.
fn render_atom_key(key: &AtomKey) -> String {
    match &key.1 {
        Some(arch) => format!("{}({})", key.0, arch),
        None => key.0.clone(),
    }
}

/// Atom-name access without owning the string. Used by RPM322/RPM323
/// where the only consumers are equality compares and `format!`
/// arguments — no need to allocate per atom.
fn atom_name_literal(atom: &DepAtom) -> Option<&str> {
    atom.name.literal_str().map(str::trim)
}

/// Yield every `DepAtom` reachable from `items` whose enclosing tag
/// passes `tag_matcher`, together with that `PreambleItem`'s span.
/// Centralises the "walk preamble → match tag → unfold rich deps"
/// loop that all four rules in this file share.
fn for_each_atom<'a, F, G>(items: &[&'a PreambleItem<Span>], tag_matcher: F, mut yield_atom: G)
where
    F: Fn(&Tag) -> bool,
    G: FnMut(Span, &'a DepAtom),
{
    for item in items {
        if !tag_matcher(&item.tag) {
            continue;
        }
        let TagValue::Dep(expr) = &item.value else {
            continue;
        };
        unfold(expr, item.data, &mut yield_atom);
    }
}

fn unfold<'a, G>(expr: &'a DepExpr, span: Span, yield_atom: &mut G)
where
    G: FnMut(Span, &'a DepAtom),
{
    match expr {
        DepExpr::Atom(a) => yield_atom(span, a),
        DepExpr::Rich(b) => unfold_bool(b, span, yield_atom),
        _ => {}
    }
}

fn unfold_bool<'a, G>(b: &'a BoolDep, span: Span, yield_atom: &mut G)
where
    G: FnMut(Span, &'a DepAtom),
{
    match b {
        BoolDep::And(xs) | BoolDep::Or(xs) | BoolDep::With(xs) => {
            for x in xs {
                unfold(x, span, yield_atom);
            }
        }
        BoolDep::If {
            cond,
            then,
            otherwise,
        }
        | BoolDep::Unless {
            cond,
            then,
            otherwise,
        } => {
            unfold(cond, span, yield_atom);
            unfold(then, span, yield_atom);
            if let Some(o) = otherwise {
                unfold(o, span, yield_atom);
            }
        }
        BoolDep::Without { left, right } => {
            unfold(left, span, yield_atom);
            unfold(right, span, yield_atom);
        }
        _ => {}
    }
}

// =====================================================================
// RPM320 duplicate-dependency-atom
// =====================================================================

pub static DUPLICATE_ATOM_METADATA: LintMetadata = LintMetadata {
    id: "RPM320",
    name: "duplicate-dependency-atom",
    description: "Same dependency atom appears more than once inside one tag's value list. \
                  RPM keeps one and ignores the rest; remove the duplicate.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct DuplicateDependencyAtom {
    diagnostics: Vec<Diagnostic>,
}

impl DuplicateDependencyAtom {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Tag-class entry: predicate matching the tag plus its human-
/// readable label. Aliased so the const-array declarations below
/// don't trip `clippy::type_complexity`.
type TagClass = (fn(&Tag) -> bool, &'static str);

/// Tag classes RPM320 inspects, paired with their human label.
const DEP_CLASSES: &[TagClass] = &[
    (|t| matches!(t, Tag::Requires), "Requires"),
    (|t| matches!(t, Tag::BuildRequires), "BuildRequires"),
    (|t| matches!(t, Tag::Provides), "Provides"),
    (|t| matches!(t, Tag::Conflicts), "Conflicts"),
    (|t| matches!(t, Tag::Obsoletes), "Obsoletes"),
    (|t| matches!(t, Tag::Recommends), "Recommends"),
    (|t| matches!(t, Tag::Suggests), "Suggests"),
    (|t| matches!(t, Tag::Supplements), "Supplements"),
    (|t| matches!(t, Tag::Enhances), "Enhances"),
];

impl<'ast> Visit<'ast> for DuplicateDependencyAtom {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Conditional-aware dedup: atoms in mutually-exclusive arms of
        // a `%if`/`%else`/`%elif` are never both live at build time and
        // must not flag each other as duplicates. Each branch inherits
        // its parent scope's `seen` set, so a duplicate is flagged only
        // when the two atoms could co-exist in the same build.
        for (matcher, label) in DEP_CLASSES {
            let initial = BTreeSet::new();
            walk_spec_items_dedup(
                &spec.items,
                *matcher,
                label,
                &initial,
                &mut self.diagnostics,
            );
            for item in &spec.items {
                if let SpecItem::Section(boxed) = item
                    && let Section::Package { content, .. } = boxed.as_ref()
                {
                    let initial = BTreeSet::new();
                    walk_preamble_content_dedup(
                        content,
                        *matcher,
                        label,
                        &initial,
                        &mut self.diagnostics,
                    );
                }
            }
        }
    }
}

/// Recurse over the main package's top-level item tree, flagging
/// duplicate atoms on tags matching `matcher`. Each conditional branch
/// gets a fresh `seen` set cloned from its parent — so atoms in
/// mutually-exclusive arms don't conflict, but a duplicate that the
/// parent scope already declared is still caught inside the arm.
fn walk_spec_items_dedup(
    items: &[SpecItem<Span>],
    matcher: fn(&Tag) -> bool,
    label: &'static str,
    parent_seen: &BTreeSet<AtomKey>,
    out: &mut Vec<Diagnostic>,
) {
    let mut local_seen = parent_seen.clone();
    for item in items {
        match item {
            SpecItem::Preamble(p) => {
                check_item_for_dup(p, matcher, label, &mut local_seen, out);
            }
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    walk_spec_items_dedup(&branch.body, matcher, label, &local_seen, out);
                }
                if let Some(els) = &c.otherwise {
                    walk_spec_items_dedup(els, matcher, label, &local_seen, out);
                }
            }
            _ => {}
        }
    }
}

/// Subpackage-flavoured walker (operates on `PreambleContent` instead
/// of `SpecItem`). Same scoping semantics as
/// [`walk_spec_items_dedup`].
fn walk_preamble_content_dedup(
    items: &[PreambleContent<Span>],
    matcher: fn(&Tag) -> bool,
    label: &'static str,
    parent_seen: &BTreeSet<AtomKey>,
    out: &mut Vec<Diagnostic>,
) {
    let mut local_seen = parent_seen.clone();
    for item in items {
        match item {
            PreambleContent::Item(p) => {
                check_item_for_dup(p, matcher, label, &mut local_seen, out);
            }
            PreambleContent::Conditional(c) => {
                for branch in &c.branches {
                    walk_preamble_content_dedup(&branch.body, matcher, label, &local_seen, out);
                }
                if let Some(els) = &c.otherwise {
                    walk_preamble_content_dedup(els, matcher, label, &local_seen, out);
                }
            }
            _ => {}
        }
    }
}

/// Check one preamble item against the current scope's `local_seen`
/// set. Inserts new atoms; emits a `RPM320` diagnostic for any that
/// were already there. Shared between the main-package and
/// subpackage walkers.
fn check_item_for_dup(
    p: &PreambleItem<Span>,
    matcher: fn(&Tag) -> bool,
    label: &'static str,
    local_seen: &mut BTreeSet<AtomKey>,
    out: &mut Vec<Diagnostic>,
) {
    if !matcher(&p.tag) {
        return;
    }
    let TagValue::Dep(expr) = &p.value else {
        return;
    };
    unfold(expr, p.data, &mut |_span, atom| {
        let Some(key) = atom_key(atom) else {
            return;
        };
        if !local_seen.insert(key.clone()) {
            out.push(Diagnostic::new(
                &DUPLICATE_ATOM_METADATA,
                Severity::Warn,
                format!(
                    "`{label}: {atom}` is listed more than once in this package",
                    atom = render_atom_key(&key),
                ),
                p.data,
            ));
        }
    });
}

impl Lint for DuplicateDependencyAtom {
    fn metadata(&self) -> &'static LintMetadata {
        &DUPLICATE_ATOM_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM321 weak-dep-duplicates-strong-dep
// =====================================================================

pub static WEAK_DUPLICATES_STRONG_METADATA: LintMetadata = LintMetadata {
    id: "RPM321",
    name: "weak-dep-duplicates-strong-dep",
    description: "A weak dependency (Recommends/Suggests/Supplements/Enhances) names a package \
                  already covered by a strong `Requires:`. The weak entry is dead weight.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct WeakDepDuplicatesStrongDep {
    diagnostics: Vec<Diagnostic>,
}

impl WeakDepDuplicatesStrongDep {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for WeakDepDuplicatesStrongDep {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Conditional-aware shadow detection. A weak dep in scope S is
        // shadowed only if a matching `Requires:` is reachable from S
        // (declared in S itself or any ancestor). Mutually-exclusive
        // branches of `%if` are independent scopes.
        let initial = BTreeSet::new();
        walk_spec_items_weak(&spec.items, &initial, &mut self.diagnostics);
        for item in &spec.items {
            if let SpecItem::Section(boxed) = item
                && let Section::Package { content, .. } = boxed.as_ref()
            {
                let initial = BTreeSet::new();
                walk_preamble_content_weak(content, &initial, &mut self.diagnostics);
            }
        }
    }
}

/// `(Tag, human label)` for the four weak-dep tags. Returns `None` for
/// other tags so callers can skip non-weak items cheaply.
fn weak_label(tag: &Tag) -> Option<&'static str> {
    match tag {
        Tag::Recommends => Some("Recommends"),
        Tag::Suggests => Some("Suggests"),
        Tag::Supplements => Some("Supplements"),
        Tag::Enhances => Some("Enhances"),
        _ => None,
    }
}

/// Two-pass walk over a main-package item list. Pass 1 builds the
/// current scope's strong set (this scope's own `Requires:` atoms,
/// without descending into conditionals — branches are independent).
/// Pass 2 checks weak atoms in this scope against the strong set, and
/// recurses into conditional branches with that strong set as the new
/// parent.
fn walk_spec_items_weak(
    items: &[SpecItem<Span>],
    parent_strong: &BTreeSet<AtomKey>,
    out: &mut Vec<Diagnostic>,
) {
    let mut local_strong = parent_strong.clone();
    for item in items {
        if let SpecItem::Preamble(p) = item
            && matches!(p.tag, Tag::Requires)
            && let TagValue::Dep(expr) = &p.value
        {
            unfold(expr, p.data, &mut |_, atom| {
                if let Some(key) = atom_key(atom) {
                    local_strong.insert(key);
                }
            });
        }
    }
    for item in items {
        match item {
            SpecItem::Preamble(p) => {
                if let Some(label) = weak_label(&p.tag) {
                    check_weak_item(p, label, &local_strong, out);
                }
            }
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    walk_spec_items_weak(&branch.body, &local_strong, out);
                }
                if let Some(els) = &c.otherwise {
                    walk_spec_items_weak(els, &local_strong, out);
                }
            }
            _ => {}
        }
    }
}

/// Subpackage-flavoured weak/strong walker; same semantics as
/// [`walk_spec_items_weak`] but for `PreambleContent` trees.
fn walk_preamble_content_weak(
    items: &[PreambleContent<Span>],
    parent_strong: &BTreeSet<AtomKey>,
    out: &mut Vec<Diagnostic>,
) {
    let mut local_strong = parent_strong.clone();
    for item in items {
        if let PreambleContent::Item(p) = item
            && matches!(p.tag, Tag::Requires)
            && let TagValue::Dep(expr) = &p.value
        {
            unfold(expr, p.data, &mut |_, atom| {
                if let Some(key) = atom_key(atom) {
                    local_strong.insert(key);
                }
            });
        }
    }
    for item in items {
        match item {
            PreambleContent::Item(p) => {
                if let Some(label) = weak_label(&p.tag) {
                    check_weak_item(p, label, &local_strong, out);
                }
            }
            PreambleContent::Conditional(c) => {
                for branch in &c.branches {
                    walk_preamble_content_weak(&branch.body, &local_strong, out);
                }
                if let Some(els) = &c.otherwise {
                    walk_preamble_content_weak(els, &local_strong, out);
                }
            }
            _ => {}
        }
    }
}

/// Emit a `RPM321` diagnostic for each weak-dep atom that is already
/// in `strong`. Shared between the main-package and subpackage
/// walkers.
fn check_weak_item(
    p: &PreambleItem<Span>,
    label: &'static str,
    strong: &BTreeSet<AtomKey>,
    out: &mut Vec<Diagnostic>,
) {
    let TagValue::Dep(expr) = &p.value else {
        return;
    };
    unfold(expr, p.data, &mut |_, atom| {
        let Some(key) = atom_key(atom) else {
            return;
        };
        if strong.contains(&key) {
            let rendered = render_atom_key(&key);
            out.push(Diagnostic::new(
                &WEAK_DUPLICATES_STRONG_METADATA,
                Severity::Warn,
                format!(
                    "`{label}: {rendered}` is shadowed by an existing \
                     `Requires: {rendered}` — drop the weak dep",
                ),
                p.data,
            ));
        }
    });
}

impl Lint for WeakDepDuplicatesStrongDep {
    fn metadata(&self) -> &'static LintMetadata {
        &WEAK_DUPLICATES_STRONG_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM322 self-weak-dependency
// =====================================================================

pub static SELF_WEAK_METADATA: LintMetadata = LintMetadata {
    id: "RPM322",
    name: "self-weak-dependency",
    description: "A weak dependency names the package itself. RPM treats self-dependencies as \
                  no-ops; the entry is almost always copy-paste from another spec.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct SelfWeakDependency {
    diagnostics: Vec<Diagnostic>,
}

impl SelfWeakDependency {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SelfWeakDependency {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            let Some(pkg_name) = pkg.name() else {
                continue;
            };
            let any_weak = |t: &Tag| {
                matches!(
                    t,
                    Tag::Recommends | Tag::Suggests | Tag::Supplements | Tag::Enhances
                )
            };
            for_each_atom(pkg.items(), any_weak, |item_span, atom| {
                // Self-weak-dep is `Name: foo` + `Recommends: foo`
                // (no arch qualifier). An atom with an arch is
                // capability-style and conceptually different — don't
                // flag.
                if atom.arch.is_some() {
                    return;
                }
                let Some(name) = atom_name_literal(atom) else {
                    return;
                };
                if name == pkg_name {
                    self.diagnostics.push(Diagnostic::new(
                        &SELF_WEAK_METADATA,
                        Severity::Warn,
                        format!(
                            "package `{pkg_name}` has a weak dependency on itself; drop the entry",
                        ),
                        item_span,
                    ));
                }
            });
        }
    }
}

impl Lint for SelfWeakDependency {
    fn metadata(&self) -> &'static LintMetadata {
        &SELF_WEAK_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM323 runtime-requires-looks-like-build-requires
// =====================================================================

pub static RUNTIME_LOOKS_BUILD_METADATA: LintMetadata = LintMetadata {
    id: "RPM323",
    name: "runtime-requires-looks-like-build-requires",
    description: "`Requires:` mentions a build-only tool (`gcc`, `cmake`, a `*-devel` package, \
                  a `pkgconfig(...)` capability, …). Move it to `BuildRequires:`.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct RuntimeRequiresLooksLikeBuildRequires {
    diagnostics: Vec<Diagnostic>,
}

impl RuntimeRequiresLooksLikeBuildRequires {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Tool / capability names that are unmistakably build-time. Kept
/// short and high-signal — the rule's purpose is to catch obvious
/// `Requires: gcc` mistakes, not to second-guess maintainers who
/// genuinely want a compiler at runtime (rare; they can `--allow`).
const BUILD_TOOL_NAMES: &[&str] = &[
    "gcc", "g++", "clang", "make", "cmake", "meson", "ninja", "autoconf", "automake", "libtool",
    "rustc", "cargo",
];

impl<'ast> Visit<'ast> for RuntimeRequiresLooksLikeBuildRequires {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            for_each_atom(
                pkg.items(),
                |t| matches!(t, Tag::Requires),
                |item_span, atom| {
                    let Some(name) = atom_name_literal(atom) else {
                        return;
                    };
                    let has_arch = atom.arch.is_some();
                    let Some(reason) = build_smell(name, has_arch) else {
                        return;
                    };
                    let rendered =
                        if let Some(arch) = atom.arch.as_ref().and_then(|t| t.literal_str()) {
                            format!("{name}({arch})")
                        } else {
                            name.to_owned()
                        };
                    self.diagnostics.push(Diagnostic::new(
                        &RUNTIME_LOOKS_BUILD_METADATA,
                        Severity::Warn,
                        format!(
                            "`Requires: {rendered}` looks like a build-time dependency \
                             ({reason}); move it to `BuildRequires:`"
                        ),
                        item_span,
                    ));
                },
            );
        }
    }
}

/// Decide whether `name` (the dep-atom name before the optional
/// `(qualifier)`) looks build-only. `has_arch` is `true` for
/// `name(qualifier)` atoms — `pkgconfig(openssl)` parses with
/// `name = "pkgconfig"` and a non-empty arch slot.
///
/// Caveat: when the qualifier contains a `.` (`pkgconfig(glib-2.0)`)
/// the upstream parser keeps the full `pkgconfig(glib-2.0)` as the
/// atom name and leaves arch `None` — that variant currently slips
/// through this rule. The mismatch is upstream; documenting it here
/// so the next contributor doesn't try to "fix" it by adding a
/// `starts_with("pkgconfig(")` branch that would double-flag the
/// arch-style hits.
fn build_smell(name: &str, has_arch: bool) -> Option<&'static str> {
    // Capability-style atoms (`pkgconfig(openssl)`, `cmake(Foo)`)
    // first — they parse with a build-tool name plus a non-empty
    // arch slot, and the more specific message ("X(...) capability")
    // beats the generic "known build tool" fallback.
    if has_arch {
        match name {
            "pkgconfig" => return Some("pkgconfig(...) capability"),
            "cmake" => return Some("cmake(...) capability"),
            _ => {}
        }
    }
    if BUILD_TOOL_NAMES.contains(&name) {
        return Some("known build tool");
    }
    if name == "pkgconfig" {
        return Some("pkgconfig build helper");
    }
    if name.ends_with("-devel") || name.ends_with("-dev") {
        return Some("development subpackage");
    }
    if name.ends_with("-static") {
        return Some("static-library subpackage");
    }
    None
}

impl Lint for RuntimeRequiresLooksLikeBuildRequires {
    fn metadata(&self) -> &'static LintMetadata {
        &RUNTIME_LOOKS_BUILD_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_320(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = DuplicateDependencyAtom::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_321(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = WeakDepDuplicatesStrongDep::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_322(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SelfWeakDependency::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_323(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = RuntimeRequiresLooksLikeBuildRequires::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM320 -----

    #[test]
    fn rpm320_flags_duplicate_requires() {
        let src = "Name: x\nRequires: foo\nRequires: foo\n";
        let diags = run_320(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM320");
        assert!(diags[0].message.contains("foo"));
    }

    #[test]
    fn rpm320_flags_duplicate_in_one_line_list() {
        let src = "Name: x\nRequires: foo, bar, foo\n";
        let diags = run_320(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn rpm320_silent_for_distinct_atoms() {
        let src = "Name: x\nRequires: foo, bar\n";
        assert!(run_320(src).is_empty());
    }

    #[test]
    fn rpm320_flags_duplicate_buildrequires() {
        let src = "Name: x\nBuildRequires: foo\nBuildRequires: foo\n";
        let diags = run_320(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("BuildRequires"));
    }

    #[test]
    fn rpm320_silent_across_different_tags() {
        // `Requires: foo` and `BuildRequires: foo` are separate
        // namespaces; not a duplicate.
        let src = "Name: x\nRequires: foo\nBuildRequires: foo\n";
        assert!(run_320(src).is_empty());
    }

    #[test]
    fn rpm320_silent_for_distinct_pkgconfig_capabilities() {
        // The bug RPM320 was rewritten to fix: `pkgconfig(openssl)`
        // and `pkgconfig(zlib)` both parse with `name = "pkgconfig"`,
        // but the arch qualifier distinguishes them.
        let src = "Name: x\nRequires: pkgconfig(openssl)\nRequires: pkgconfig(zlib)\n";
        assert!(run_320(src).is_empty(), "{:?}", run_320(src));
    }

    #[test]
    fn rpm320_flags_identical_pkgconfig_capability() {
        let src = "Name: x\nRequires: pkgconfig(openssl)\nRequires: pkgconfig(openssl)\n";
        let diags = run_320(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("pkgconfig(openssl)"));
    }

    #[test]
    fn rpm320_flags_duplicate_inside_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
Requires: foo\n\
Requires: foo\n\
%description devel\nbody\n";
        let diags = run_320(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn rpm320_silent_for_same_atom_in_mutually_exclusive_branches() {
        // Real-world ALT spec: `BuildRequires: gcc-c++` repeated in
        // every arm of nested `%if`/`%else`. Each arm is independently
        // active at build time — no duplicate.
        let src = "Name: x\n\
%if 0%{?fedora}\n\
BuildRequires: gcc-c++\n\
%else\n\
BuildRequires: gcc-c++\n\
%endif\n";
        assert!(run_320(src).is_empty(), "{:?}", run_320(src));
    }

    #[test]
    fn rpm320_silent_for_same_atom_in_nested_elif_chain() {
        // Nested elif chain — all arms mutually exclusive.
        let src = "Name: x\n\
%if 0%{?suse_version}\n\
BuildRequires: gcc-c++\n\
%elif 0%{?fedora}\n\
BuildRequires: gcc-c++\n\
%elif 0%{?rhel}\n\
BuildRequires: gcc-c++\n\
%else\n\
BuildRequires: gcc-c++\n\
%endif\n";
        assert!(run_320(src).is_empty(), "{:?}", run_320(src));
    }

    #[test]
    fn rpm320_flags_duplicate_inside_one_branch() {
        // Two BuildRequires: foo lines inside the SAME branch — still
        // a real duplicate.
        let src = "Name: x\n\
%if 0%{?fedora}\n\
BuildRequires: gcc-c++\n\
BuildRequires: gcc-c++\n\
%endif\n";
        let diags = run_320(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm320_flags_duplicate_when_parent_scope_already_has_atom() {
        // `BuildRequires: foo` at top level + `BuildRequires: foo`
        // inside `%if` — in the branch where %if is true, foo is
        // declared twice. Flag inside the arm.
        let src = "Name: x\n\
BuildRequires: foo\n\
%if 0%{?fedora}\n\
BuildRequires: foo\n\
%endif\n";
        assert_eq!(run_320(src).len(), 1);
    }

    // ----- RPM321 -----

    #[test]
    fn rpm321_flags_recommends_shadowed_by_requires() {
        let src = "Name: x\nRequires: foo\nRecommends: foo\n";
        let diags = run_321(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM321");
    }

    #[test]
    fn rpm321_flags_suggests_shadowed() {
        let src = "Name: x\nRequires: foo\nSuggests: foo\n";
        assert_eq!(run_321(src).len(), 1);
    }

    #[test]
    fn rpm321_flags_supplements_shadowed() {
        let src = "Name: x\nRequires: foo\nSupplements: foo\n";
        let diags = run_321(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Supplements"));
    }

    #[test]
    fn rpm321_flags_enhances_shadowed() {
        let src = "Name: x\nRequires: foo\nEnhances: foo\n";
        let diags = run_321(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Enhances"));
    }

    #[test]
    fn rpm321_silent_when_weak_is_distinct() {
        let src = "Name: x\nRequires: foo\nRecommends: bar\n";
        assert!(run_321(src).is_empty());
    }

    #[test]
    fn rpm321_silent_without_requires() {
        let src = "Name: x\nRecommends: foo\n";
        assert!(run_321(src).is_empty());
    }

    #[test]
    fn rpm321_silent_for_distinct_pkgconfig_capabilities() {
        // pkgconfig(openssl) vs pkgconfig(zlib) — distinct keys.
        let src = "Name: x\nRequires: pkgconfig(openssl)\nRecommends: pkgconfig(zlib)\n";
        assert!(run_321(src).is_empty(), "{:?}", run_321(src));
    }

    #[test]
    fn rpm321_silent_when_strong_and_weak_in_mutually_exclusive_branches() {
        // `Requires: foo` in one arm and `Recommends: foo` in the other
        // — never both live at once, so no shadowing.
        let src = "Name: x\n\
%if 0%{?fedora}\n\
Requires: foo\n\
%else\n\
Recommends: foo\n\
%endif\n";
        assert!(run_321(src).is_empty(), "{:?}", run_321(src));
    }

    #[test]
    fn rpm321_flags_when_strong_in_parent_scope_covers_weak_in_branch() {
        // `Requires: foo` at top level + `Recommends: foo` in `%if`:
        // when %if fires, both are present → shadow.
        let src = "Name: x\nRequires: foo\n%if 0%{?fedora}\nRecommends: foo\n%endif\n";
        assert_eq!(run_321(src).len(), 1);
    }

    // ----- RPM322 -----

    #[test]
    fn rpm322_flags_self_recommends() {
        let src = "Name: foo\nRecommends: foo\n";
        let diags = run_322(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM322");
    }

    #[test]
    fn rpm322_flags_self_suggests() {
        let src = "Name: foo\nSuggests: foo\n";
        assert_eq!(run_322(src).len(), 1);
    }

    #[test]
    fn rpm322_flags_self_supplements() {
        let src = "Name: foo\nSupplements: foo\n";
        assert_eq!(run_322(src).len(), 1);
    }

    #[test]
    fn rpm322_flags_self_enhances() {
        let src = "Name: foo\nEnhances: foo\n";
        assert_eq!(run_322(src).len(), 1);
    }

    #[test]
    fn rpm322_silent_for_other_package() {
        let src = "Name: foo\nRecommends: bar\n";
        assert!(run_322(src).is_empty());
    }

    #[test]
    fn rpm322_silent_when_strong_self_dep() {
        // Requires self isn't covered by RPM322; that's a separate
        // smell (RPM033 self-obsoletion / similar). Make sure 322
        // only fires on weak deps.
        let src = "Name: foo\nRequires: foo\n";
        assert!(run_322(src).is_empty());
    }

    #[test]
    fn rpm322_silent_for_capability_with_same_name() {
        // `Name: foo` + `Recommends: foo(bar)` — capability has an
        // arch qualifier, so it's conceptually a different thing
        // from the package itself.
        let src = "Name: foo\nRecommends: foo(bar)\n";
        assert!(run_322(src).is_empty());
    }

    // ----- RPM323 -----

    #[test]
    fn rpm323_flags_gcc_in_requires() {
        let src = "Name: x\nRequires: gcc\n";
        let diags = run_323(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM323");
        assert!(diags[0].message.contains("build tool"));
    }

    #[test]
    fn rpm323_flags_meson_in_requires() {
        let src = "Name: x\nRequires: meson\n";
        assert_eq!(run_323(src).len(), 1);
    }

    #[test]
    fn rpm323_flags_cargo_in_requires() {
        let src = "Name: x\nRequires: cargo\n";
        assert_eq!(run_323(src).len(), 1);
    }

    #[test]
    fn rpm323_flags_devel_subpackage() {
        let src = "Name: x\nRequires: openssl-devel\n";
        let diags = run_323(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("development"));
    }

    #[test]
    fn rpm323_flags_dev_subpackage() {
        let src = "Name: x\nRequires: libfoo-dev\n";
        let diags = run_323(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("development"));
    }

    #[test]
    fn rpm323_flags_static_subpackage() {
        let src = "Name: x\nRequires: libfoo-static\n";
        let diags = run_323(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("static-library"));
    }

    #[test]
    fn rpm323_flags_pkgconfig_capability() {
        let src = "Name: x\nRequires: pkgconfig(openssl)\n";
        let diags = run_323(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("pkgconfig(openssl)"));
    }

    #[test]
    fn rpm323_flags_cmake_capability() {
        let src = "Name: x\nRequires: cmake(Foo)\n";
        let diags = run_323(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("cmake(...)"));
    }

    #[test]
    fn rpm323_silent_for_runtime_dep() {
        let src = "Name: x\nRequires: glibc\n";
        assert!(run_323(src).is_empty());
    }

    #[test]
    fn rpm323_silent_for_buildrequires() {
        // BuildRequires: gcc is correct — RPM323 only inspects Requires.
        let src = "Name: x\nBuildRequires: gcc\n";
        assert!(run_323(src).is_empty());
    }
}
