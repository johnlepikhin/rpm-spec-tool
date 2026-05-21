//! Bcond hygiene: RPM401 + RPM402.
//!
//! - **RPM401 `bcond-defined-but-unused`** — a `%bcond_with NAME` /
//!   `%bcond_without NAME` / `%bcond NAME DEFAULT` declaration without
//!   any matching `%{with NAME}` / `%{without NAME}` reference. Dead
//!   build toggles confuse the spec audit ("did this flag ever do
//!   anything?") and slow down `mock --without` invocations because
//!   the option silently has no effect.
//! - **RPM402 `with-condition-without-bcond`** — `%{with NAME}` or
//!   `%{without NAME}` referenced where no `%bcond[_with|_without]
//!   NAME` was declared. RPM treats an undeclared `%{with foo}` as
//!   false-ish (the macro expands empty), so the branch silently
//!   never fires — a bug-prone setup the maintainer almost certainly
//!   didn't intend.
//!
//! Both rules walk the spec once via the [`Visit`] trait to collect
//! the declared and referenced bcond names, then emit the two
//! set-difference diagnostics. Names are compared verbatim — that's
//! how RPM resolves them at build time.

use std::collections::BTreeMap;

use rpm_spec::ast::{
    BoolDep, BuildCondStyle, BuildCondition, CondExpr, Conditional, DepExpr, ExprAst, FileTrigger,
    MacroRef, PreambleContent, PreambleItem, Scriptlet, Section, ShellBody, Span, SpecFile,
    SpecItem, TagValue, Text, TextBody, TextSegment, Trigger,
};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

/// Macro head names that mark a `%{with NAME}` / `%{without NAME}`
/// reference. Used both when probing parsed `MacroRef` nodes and when
/// scanning verbatim macro text inside legacy `CondExpr::Raw` bodies.
const WITH_NAMES: &[&str] = &["with", "without"];

// =====================================================================
// RPM401 bcond-defined-but-unused
// =====================================================================

pub static DEFINED_BUT_UNUSED_METADATA: LintMetadata = LintMetadata {
    id: "RPM401",
    name: "bcond-defined-but-unused",
    description: "A `%bcond` / `%bcond_with` / `%bcond_without` declaration has no matching \
                  `%{with name}` / `%{without name}` reference anywhere in the spec. The toggle \
                  has no effect; remove it or wire it up.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Lint state for RPM401. Collects every bcond declaration with its
/// span; checked against the referenced set at end of visit.
#[derive(Debug, Default)]
pub struct BcondDefinedButUnused {
    diagnostics: Vec<Diagnostic>,
}

impl BcondDefinedButUnused {
    /// Construct an empty lint instance with no diagnostics buffered.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for BcondDefinedButUnused {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let usage = BcondUsage::collect(spec);
        for (name, decl) in &usage.declared {
            if usage.referenced.contains_key(name.as_str()) {
                continue;
            }
            self.diagnostics.push(Diagnostic::new(
                &DEFINED_BUT_UNUSED_METADATA,
                Severity::Warn,
                format!(
                    "bcond `{name}` is declared but never referenced via `%{{with {name}}}` or \
                     `%{{without {name}}}`; the toggle has no effect"
                ),
                decl.span,
            ));
        }
    }
}

impl Lint for BcondDefinedButUnused {
    fn metadata(&self) -> &'static LintMetadata {
        &DEFINED_BUT_UNUSED_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM402 with-condition-without-bcond
// =====================================================================

pub static REF_WITHOUT_BCOND_METADATA: LintMetadata = LintMetadata {
    id: "RPM402",
    name: "with-condition-without-bcond",
    description: "A `%{with name}` or `%{without name}` reference has no matching `%bcond` \
                  declaration. RPM expands the reference to nothing, so the conditional silently \
                  never fires — declare the bcond or fix the typo.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Lint state for RPM402.
#[derive(Debug, Default)]
pub struct WithConditionWithoutBcond {
    diagnostics: Vec<Diagnostic>,
}

impl WithConditionWithoutBcond {
    /// Construct an empty lint instance with no diagnostics buffered.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for WithConditionWithoutBcond {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let usage = BcondUsage::collect(spec);
        // Dedup by name — multiple references to the same undeclared
        // bcond yield one diagnostic at the first reference span.
        for (name, reference) in &usage.referenced {
            if usage.declared.contains_key(name) {
                continue;
            }
            self.diagnostics.push(Diagnostic::new(
                &REF_WITHOUT_BCOND_METADATA,
                Severity::Warn,
                format!(
                    "reference to `%{{with {name}}}` / `%{{without {name}}}` without a matching \
                     `%bcond_with {name}` / `%bcond_without {name}` declaration; RPM expands \
                     it to nothing — the branch silently never fires"
                ),
                reference.span,
            ));
        }
    }
}

impl Lint for WithConditionWithoutBcond {
    fn metadata(&self) -> &'static LintMetadata {
        &REF_WITHOUT_BCOND_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// Shared collector
// =====================================================================

/// Polarity of a `%bcond*` declaration. Preserved so cross-rule
/// consumers (e.g. a future "redundant `%{with}` in default-on bcond"
/// lint) can reason about default state without re-parsing the AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BcondPolarity {
    /// `%bcond_with NAME` — default off.
    With,
    /// `%bcond_without NAME` — default on.
    Without,
    /// `%bcond NAME DEFAULT` (rpm ≥ 4.17.1) — default expression
    /// determines polarity at runtime; we only record the syntactic form.
    Modern,
}

impl BcondPolarity {
    fn from_style(style: BuildCondStyle) -> Self {
        match style {
            BuildCondStyle::BcondWith => Self::With,
            BuildCondStyle::BcondWithout => Self::Without,
            BuildCondStyle::Bcond => Self::Modern,
            // `BuildCondStyle` is `#[non_exhaustive]` in the rpm-spec
            // crate. Future variants default to Modern until we have
            // a reason to distinguish them — the polarity field is
            // future-proofing for cross-rule queries today.
            _ => Self::Modern,
        }
    }
}

/// Polarity of a `%{with NAME}` / `%{without NAME}` reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WithKind {
    With,
    Without,
}

impl WithKind {
    fn from_head(head: &str) -> Option<Self> {
        match head {
            "with" => Some(Self::With),
            "without" => Some(Self::Without),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BcondDecl {
    #[expect(
        dead_code,
        reason = "future-proof for cross-rule queries that distinguish bcond polarity"
    )]
    polarity: BcondPolarity,
    span: Span,
}

#[derive(Debug, Clone, Copy)]
struct BcondRef {
    #[expect(
        dead_code,
        reason = "future-proof for cross-rule queries that distinguish with/without references"
    )]
    kind: WithKind,
    span: Span,
}

/// Pair of "what was declared" + "what was referenced" for a spec.
///
/// Names are stored as `String` because the AST owns them (`%bcond`
/// declarations store the name as `String`; macro references as
/// `String`). Spans on declarations are the `BuildCondition.data`
/// (`Span` from the parser).
#[derive(Debug, Default)]
struct BcondUsage {
    /// `name → declaration`. Last write wins on duplicate names; a
    /// separate rule could flag redefinition.
    declared: BTreeMap<String, BcondDecl>,
    /// `name → first reference`. Used as the diagnostic anchor for
    /// RPM402.
    referenced: BTreeMap<String, BcondRef>,
}

impl BcondUsage {
    fn collect(spec: &SpecFile<Span>) -> Self {
        let mut out = BcondUsage::default();
        walk_items(&spec.items, &mut out);
        out
    }

    fn record_ref(&mut self, name: String, kind: WithKind, span: Span) {
        self.referenced
            .entry(name)
            .or_insert(BcondRef { kind, span });
    }
}

/// Walk `&[SpecItem<Span>]` recursively, descending into conditionals
/// and subpackage sections, recording bcond declarations and `%{with
/// NAME}` / `%{without NAME}` references.
fn walk_items(items: &[SpecItem<Span>], out: &mut BcondUsage) {
    for item in items {
        match item {
            SpecItem::BuildCondition(b) => record_declaration(out, b),
            SpecItem::Preamble(p) => walk_preamble_item(p, out),
            SpecItem::Conditional(c) => walk_top_cond(c, out),
            SpecItem::Section(boxed) => walk_section(boxed.as_ref(), out),
            SpecItem::MacroDef(m) => walk_text_for_refs(&m.body, m.data, out),
            SpecItem::Statement(m) => {
                walk_macro_ref(m, out, items_anchor(items).unwrap_or_default())
            }
            _ => {}
        }
    }
}

fn walk_top_cond(cond: &Conditional<Span, SpecItem<Span>>, out: &mut BcondUsage) {
    for branch in &cond.branches {
        walk_cond_expr(&branch.expr, cond.data, out);
        walk_items(&branch.body, out);
    }
    if let Some(els) = &cond.otherwise {
        walk_items(els, out);
    }
}

/// Walk a `CondExpr` for `%{with foo}` / `%{without foo}` references.
/// The parser uses two shapes — structured `Parsed(ExprAst)` and
/// legacy `Raw(Text)` — both can carry macro references and both are
/// covered here.
fn walk_cond_expr(expr: &CondExpr<Span>, anchor: Span, out: &mut BcondUsage) {
    match expr {
        CondExpr::Raw(t) => walk_text_for_refs(t, anchor, out),
        CondExpr::Parsed(ast) => walk_expr_ast(ast, out),
        CondExpr::ArchList(items) => {
            for t in items {
                walk_text_for_refs(t, anchor, out);
            }
        }
        _ => {}
    }
}

/// Walk an `ExprAst` for `%{with foo}` / `%{without foo}` macros.
/// `ExprAst::Macro` stores the macro verbatim as a `String`; parse
/// it via the shared [`rpm_spec::ast::parse_bcond_verbatim`] so the
/// detection rule lives in one place. Each node carries its own
/// span via the `data` field, so no anchor parameter is needed.
fn walk_expr_ast(ast: &ExprAst<Span>, out: &mut BcondUsage) {
    match ast {
        #[allow(clippy::collapsible_match)]
        ExprAst::Macro { text, data } => {
            if let Some((form, name)) = rpm_spec::ast::parse_bcond_verbatim(text) {
                let kind = match form {
                    rpm_spec::ast::BcondForm::With => WithKind::With,
                    rpm_spec::ast::BcondForm::Without => WithKind::Without,
                    // `BcondForm` is `#[non_exhaustive]` upstream; an
                    // unknown variant is most safely treated as
                    // "not a bcond reference" so we don't conjure a
                    // WithKind we can't justify.
                    _ => return,
                };
                out.record_ref(name.to_owned(), kind, *data);
            }
        }
        ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => {
            walk_expr_ast(inner, out);
        }
        ExprAst::Binary { lhs, rhs, .. } => {
            walk_expr_ast(lhs, out);
            walk_expr_ast(rhs, out);
        }
        _ => {}
    }
}

fn walk_section(section: &Section<Span>, out: &mut BcondUsage) {
    match section {
        Section::Package { content, .. } => walk_preamble_content(content, out),
        // Script-bearing sections may host `%if %{with NAME}` lines —
        // the parser keeps the conditional as plain text, so the
        // `%{with NAME}` macro reference shows up as a
        // `TextSegment::Macro` on one of the body lines.
        Section::BuildScript { body, data, .. }
        | Section::Verify { body, data, .. }
        | Section::Sepolicy { body, data, .. } => walk_shell_body(body, *data, out),
        Section::Scriptlet(s) => walk_scriptlet(s, out),
        Section::Trigger(t) => walk_trigger(t, out),
        Section::FileTrigger(t) => walk_file_trigger(t, out),
        Section::Description { body, data, .. } => walk_text_body(body, *data, out),
        // `Files`, `Changelog`, `SourceList`, `PatchList` are unlikely
        // to host `%{with NAME}` references in practice; their bodies
        // are not RPM-language `%if` hosts.
        _ => {}
    }
}

fn walk_shell_body(body: &ShellBody<Span>, anchor: Span, out: &mut BcondUsage) {
    for line in &body.lines {
        walk_text_for_refs(line, anchor, out);
    }
}

fn walk_text_body(body: &TextBody, anchor: Span, out: &mut BcondUsage) {
    for line in &body.lines {
        walk_text_for_refs(line, anchor, out);
    }
}

fn walk_scriptlet(s: &Scriptlet<Span>, out: &mut BcondUsage) {
    walk_shell_body(&s.body, s.data, out);
}

fn walk_trigger(t: &Trigger<Span>, out: &mut BcondUsage) {
    walk_shell_body(&t.body, t.data, out);
}

fn walk_file_trigger(t: &FileTrigger<Span>, out: &mut BcondUsage) {
    walk_shell_body(&t.body, t.data, out);
}

fn walk_preamble_content(items: &[PreambleContent<Span>], out: &mut BcondUsage) {
    for item in items {
        match item {
            PreambleContent::Item(p) => walk_preamble_item(p, out),
            PreambleContent::Conditional(c) => {
                for branch in &c.branches {
                    walk_cond_expr(&branch.expr, c.data, out);
                    walk_preamble_content(&branch.body, out);
                }
                if let Some(els) = &c.otherwise {
                    walk_preamble_content(els, out);
                }
            }
            _ => {}
        }
    }
}

fn walk_preamble_item(p: &PreambleItem<Span>, out: &mut BcondUsage) {
    let anchor = p.data;
    match &p.value {
        TagValue::Text(t) => walk_text_for_refs(t, anchor, out),
        TagValue::ArchList(items) => {
            for t in items {
                walk_text_for_refs(t, anchor, out);
            }
        }
        // `TagValue::Dep` — `Requires`, `BuildRequires`, etc. The
        // atom name is a `Text`, so a `%{with foo}` macro reference
        // shows up as a `TextSegment::Macro` we can walk. Rich
        // boolean deps are recursed into.
        TagValue::Dep(dep) => walk_dep_expr(dep, anchor, out),
        _ => {}
    }
}

fn walk_dep_expr(dep: &DepExpr, anchor: Span, out: &mut BcondUsage) {
    match dep {
        DepExpr::Atom(atom) => {
            walk_text_for_refs(&atom.name, anchor, out);
            if let Some(arch) = &atom.arch {
                walk_text_for_refs(arch, anchor, out);
            }
            if let Some(c) = &atom.constraint {
                walk_text_for_refs(&c.evr.version, anchor, out);
                if let Some(rel) = &c.evr.release {
                    walk_text_for_refs(rel, anchor, out);
                }
            }
        }
        DepExpr::Rich(bool_dep) => walk_bool_dep(bool_dep, anchor, out),
        // `DepExpr` is `#[non_exhaustive]`; future variants are
        // ignored until they exist.
        _ => {}
    }
}

fn walk_bool_dep(node: &BoolDep, anchor: Span, out: &mut BcondUsage) {
    match node {
        BoolDep::And(children) | BoolDep::Or(children) | BoolDep::With(children) => {
            for d in children {
                walk_dep_expr(d, anchor, out);
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
            walk_dep_expr(cond, anchor, out);
            walk_dep_expr(then, anchor, out);
            if let Some(o) = otherwise {
                walk_dep_expr(o, anchor, out);
            }
        }
        BoolDep::Without { left, right } => {
            walk_dep_expr(left, anchor, out);
            walk_dep_expr(right, anchor, out);
        }
        // `BoolDep` is `#[non_exhaustive]`; future variants are
        // ignored until they exist.
        _ => {}
    }
}

fn walk_text_for_refs(text: &Text, anchor: Span, out: &mut BcondUsage) {
    for seg in &text.segments {
        if let TextSegment::Macro(m) = seg {
            walk_macro_ref(m, out, anchor);
        }
    }
}

fn walk_macro_ref(m: &MacroRef, out: &mut BcondUsage, anchor: Span) {
    if let Some((kind, name)) = with_or_without_target(m) {
        out.record_ref(name, kind, anchor);
    }
    // Macro args may themselves contain nested macro refs (e.g.
    // `%{with %{computed_name}}` — rare). Walk them too.
    for arg in &m.args {
        walk_text_for_refs(arg, anchor, out);
    }
    if let Some(t) = &m.with_value {
        walk_text_for_refs(t, anchor, out);
    }
}

/// Pick an anchor span for `SpecItem::Statement` macros — they don't
/// carry their own per-statement span in this AST shape. Returns the
/// first preamble item's span as a best-effort fallback; caller falls
/// back to `Span::default()` when nothing is anchorable.
fn items_anchor(items: &[SpecItem<Span>]) -> Option<Span> {
    for item in items {
        if let SpecItem::Preamble(p) = item {
            return Some(p.data);
        }
    }
    None
}

fn record_declaration(out: &mut BcondUsage, node: &BuildCondition<Span>) {
    // Last declaration wins on duplicate names — a separate rule could
    // flag redefinition (RPM003-style); not our concern here.
    let polarity = BcondPolarity::from_style(node.style);
    out.declared.insert(
        node.name.clone(),
        BcondDecl {
            polarity,
            span: node.data,
        },
    );
    // The default expression on `%bcond name DEFAULT` is plain Text;
    // walk it too in case it contains `%{with other}` (unlikely but
    // possible).
    if let Some(default) = &node.default {
        walk_text_for_refs(default, node.data, out);
    }
}

/// If `m` is `%{with NAME}` or `%{without NAME}`, return
/// `(WithKind, NAME)`. `%bcond_value(name)` is *not* a usage reference
/// — it's another declaration form (rpm ≥ 4.17.1), counted separately.
///
/// Handles two parser shapes uniformly:
/// * Structural form: `MacroKind::Builtin(BuiltinMacro::With)` —
///   the parser promoted the ref because the arg was a clean literal.
/// * Legacy parametric form: `MacroKind::Parametric` with
///   `name="with"`/`"without"` — surfaces for shapes the structural
///   detector couldn't normalise (e.g. args containing macro
///   segments). The lint detects both because users care equally
///   about both surface forms.
///
/// In both shapes the feature name lives in `args[0]` — the unit
/// `BuiltinMacro::With`/`Without` variants carry no payload, keeping
/// a single source of truth for the feature name.
fn with_or_without_target(m: &MacroRef) -> Option<(WithKind, String)> {
    use rpm_spec::ast::{BuiltinMacro, MacroKind};
    let kind = match &m.kind {
        MacroKind::Builtin(BuiltinMacro::With) => WithKind::With,
        MacroKind::Builtin(BuiltinMacro::Without) => WithKind::Without,
        MacroKind::Parametric if WITH_NAMES.contains(&m.name.as_str()) => {
            WithKind::from_head(m.name.as_str())?
        }
        _ => return None,
    };
    let arg = m.args.first()?;
    let lit = arg.literal_str()?;
    let trimmed = lit.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some((kind, trimmed.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_401(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = BcondDefinedButUnused::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_402(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = WithConditionWithoutBcond::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM401 -----

    #[test]
    fn rpm401_flags_unused_bcond_with() {
        let src = "%bcond_with foo\nName: x\n";
        let diags = run_401(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM401");
        assert!(diags[0].message.contains("foo"));
    }

    #[test]
    fn rpm401_flags_unused_bcond_without() {
        let src = "%bcond_without bar\nName: x\n";
        assert_eq!(run_401(src).len(), 1);
    }

    #[test]
    fn rpm401_silent_when_bcond_used_with_macro() {
        let src = "%bcond_with foo\nName: x\n\
%if %{with foo}\nBuildRequires: extra\n%endif\n";
        assert!(run_401(src).is_empty());
    }

    #[test]
    fn rpm401_silent_when_bcond_used_without_macro() {
        let src = "%bcond_without bar\nName: x\n\
%if %{without bar}\nBuildRequires: alt\n%endif\n";
        assert!(run_401(src).is_empty());
    }

    #[test]
    fn rpm401_flags_one_of_two_when_only_one_referenced() {
        let src = "%bcond_with used\n%bcond_with unused\nName: x\n\
%if %{with used}\nBuildRequires: a\n%endif\n";
        let diags = run_401(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unused"));
    }

    #[test]
    fn rpm401_silent_for_modern_bcond_form() {
        // `%bcond name DEFAULT` (rpm ≥ 4.17.1) — same usage check.
        let src = "%bcond newcond 1\nName: x\n\
%if %{with newcond}\nRequires: a\n%endif\n";
        assert!(run_401(src).is_empty());
    }

    #[test]
    fn rpm401_silent_when_bcond_used_via_optional_with() {
        // `%{?with foo}` is the tolerant optional form; RPM still
        // treats it as a `with` reference at evaluation time, so a
        // declared bcond is "used".
        let src = "%bcond_with foo\nName: x\n\
%if %{?with foo}\nBuildRequires: extra\n%endif\n";
        assert!(run_401(src).is_empty(), "{:?}", run_401(src));
    }

    #[test]
    fn silent_when_with_used_inside_build_section() {
        // `%if %{with FOO}` inside a `%build` body — the parser keeps
        // the conditional as text lines, so the macro reference must
        // still be picked up by the bcond usage walker.
        let src = "Name: x\n%bcond_with FOO\n%prep\n%build\n\
%if %{with FOO}\necho on\n%endif\n";
        let diags = run_401(src);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn silent_when_with_used_inside_check_section() {
        let src = "Name: x\n%bcond_with FOO\n%check\n\
%if %{with FOO}\nmake check\n%endif\n";
        let diags = run_401(src);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn silent_when_without_used_inside_install_section() {
        let src = "Name: x\n%bcond_with FOO\n%install\n\
%if %{without FOO}\nrm extra\n%endif\n";
        let diags = run_401(src);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn rpm401_silent_when_bcond_declared_in_subpackage_used_in_main() {
        // Declaration at top level, usage inside a subpackage's
        // preamble via `%if %{with foo}` so the walker exercises the
        // Section::Package preamble-content path through a nested
        // conditional. The bcond is referenced, so RPM401 must stay
        // silent.
        let src = "%bcond_with foo\n\
Name: x\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
%description\nmain\n\
%package devel\nSummary: d\n\
%if %{with foo}\nRequires: foo-extras\n%endif\n\
%description devel\ndevel\n";
        assert!(run_401(src).is_empty(), "{:?}", run_401(src));
    }

    // ----- RPM402 -----

    #[test]
    fn rpm402_flags_with_without_bcond() {
        let src = "Name: x\n%if %{with foo}\nBuildRequires: a\n%endif\n";
        let diags = run_402(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM402");
        assert!(diags[0].message.contains("foo"));
    }

    #[test]
    fn rpm402_flags_without_without_bcond() {
        let src = "Name: x\n%if %{without bar}\nBuildRequires: a\n%endif\n";
        assert_eq!(run_402(src).len(), 1);
    }

    #[test]
    fn rpm402_silent_when_bcond_declared() {
        let src = "%bcond_with foo\nName: x\n%if %{with foo}\nBuildRequires: a\n%endif\n";
        assert!(run_402(src).is_empty());
    }

    #[test]
    fn rpm402_deduplicates_repeated_references() {
        // Two references to the same undeclared bcond → one diagnostic.
        let src = "Name: x\n\
%if %{with foo}\nBuildRequires: a\n%endif\n\
%if %{with foo}\nBuildRequires: b\n%endif\n";
        assert_eq!(run_402(src).len(), 1);
    }

    #[test]
    fn rpm402_silent_when_bcond_modern_form_declared() {
        let src = "%bcond foo 1\nName: x\n%if %{with foo}\nBuildRequires: a\n%endif\n";
        assert!(run_402(src).is_empty());
    }

    #[test]
    fn rpm402_flags_each_distinct_missing_name() {
        let src = "Name: x\n%if %{with foo}\nBuildRequires: a\n%endif\n\
%if %{without bar}\nBuildRequires: b\n%endif\n";
        let diags = run_402(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
    }

    #[test]
    fn rpm402_silent_on_clean_spec() {
        let src = "Name: x\nVersion: 1\n";
        assert!(run_402(src).is_empty());
    }

    #[test]
    fn rpm402_flags_optional_with_without_bcond() {
        // `%{?with foo}` with no declared bcond — the optional form
        // doesn't excuse the undeclared bcond; RPM still expands it
        // to nothing, the branch silently never fires.
        let src = "Name: x\n%if %{?with foo}\nBuildRequires: a\n%endif\n";
        let diags = run_402(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("foo"));
    }

    #[test]
    fn rpm402_handles_negated_optional() {
        // `%{!?with foo}` — negated optional, still a `with`
        // reference for the purpose of bcond hygiene.
        let src = "Name: x\n%if %{!?with foo}\nBuildRequires: a\n%endif\n";
        let diags = run_402(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("foo"));
    }
}
