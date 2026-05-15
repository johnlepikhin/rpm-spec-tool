//! Phase 8c — Macro value propagation.
//!
//! Two rules sharing a definition-tracking pass:
//!
//! - **RPM117** `macro-defined-makes-if-trivial` — when a `%global` /
//!   `%define` upstream of a `%if EXPR` lets us fold `%{X}` /
//!   `%{?X}` references inside `EXPR` into a constant, the whole
//!   `%if` collapses to a known truth value.
//! - **RPM118** `unused-conditional-global` — `%global X V` defined
//!   but never read via `%{X}` / `%{?X}` / `%{!?X}` anywhere in the
//!   spec.
//!
//! ## Scope model
//!
//! Must-define reaching definitions. A snapshot of the macro table is
//! taken before entering a conditional; each branch starts from that
//! snapshot; on block exit we restore. Inside a branch, definitions
//! made earlier in the same branch are visible to later code.
//! Cross-branch merge is **not** performed — `%if A %global X 1 %else
//! %global X 2 %endif` leaves X undefined after the block, conservatively.

use std::collections::HashMap;

use rpm_spec::ast::{
    CondExpr, Conditional, ExprAst, FilesContent, MacroDef, MacroDefKind, PreambleContent,
    PreambleItem, Section, Span, SpecFile, SpecItem, Text, TextSegment,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{is_constant_false_condition, is_constant_true_condition};
use crate::visit::Visit;

// =====================================================================
// Macro reference parsing
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacroMod {
    /// `%{X}` / `%X`.
    None,
    /// `%{?X}`.
    IfDefined,
    /// `%{!?X}`.
    IfNotDefined,
}

/// Parse `%X` / `%{X}` / `%{?X}` / `%{!?X}` (with optional `:default`
/// trailer or args) into `(modifier, name)`. Returns `None` for shapes
/// we can't handle: shell macros `%(...)`, expression macros `%[...]`,
/// positional `%1`, parametric calls `%{X foo bar}`.
fn parse_macro_ref(text: &str) -> Option<(MacroMod, String)> {
    let inner = text.strip_prefix('%')?;
    // Reject `%(...)`, `%[...]` and `%%` escapes.
    if inner.starts_with('(') || inner.starts_with('[') || inner.starts_with('%') {
        return None;
    }
    let (raw, braced) = match inner.strip_prefix('{') {
        Some(rest) => (rest.strip_suffix('}')?, true),
        None => (inner, false),
    };
    let (modifier, rest) = if let Some(r) = raw.strip_prefix("!?") {
        (MacroMod::IfNotDefined, r)
    } else if let Some(r) = raw.strip_prefix('?') {
        (MacroMod::IfDefined, r)
    } else {
        (MacroMod::None, raw)
    };
    // Stop at `:` (default-value form), whitespace (parametric call),
    // or end of string.
    let name_end = rest
        .find(|c: char| c == ':' || c.is_whitespace())
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    if name.is_empty() {
        return None;
    }
    // Reject names that contain non-macro chars or are positional /
    // flag references. Names start with `_` or alphanumeric; everything
    // else (e.g. `*`, `**`, `#`, digits-only positional) we punt on.
    if !braced && (name.starts_with(|c: char| c.is_ascii_digit()) || name.contains('*')) {
        return None;
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((modifier, name.to_string()))
}

// =====================================================================
// Macro table
// =====================================================================

#[derive(Debug, Clone)]
enum MacroLiteral {
    Integer(i64),
    String(String),
}

#[derive(Debug, Clone)]
struct MacroBinding {
    /// Literal value when the body is a single-segment literal that
    /// looks like an integer or a string. `None` means defined but
    /// nontrivial (contains other macros / multi-segment / parametric).
    value: Option<MacroLiteral>,
}

#[derive(Debug, Clone, Default)]
struct MacroTable {
    map: HashMap<String, MacroBinding>,
}

impl MacroTable {
    fn insert(&mut self, m: &MacroDef<Span>) {
        if matches!(m.kind, MacroDefKind::Undefine) {
            self.map.remove(&m.name);
            return;
        }
        let value = literal_from_body(&m.body);
        self.map.insert(m.name.clone(), MacroBinding { value });
    }

    fn get(&self, name: &str) -> Option<&MacroBinding> {
        self.map.get(name)
    }
}

/// Extract a literal value from a macro body. Returns `None` for any
/// non-trivial shape (macro references, multi-segment, parametric
/// options) — conservative bail.
fn literal_from_body(body: &Text) -> Option<MacroLiteral> {
    let raw = body.literal_str()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Some(MacroLiteral::String(String::new()));
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return Some(MacroLiteral::Integer(i));
    }
    Some(MacroLiteral::String(trimmed.to_string()))
}

// =====================================================================
// Expression folding
// =====================================================================

/// Walk `expr`, substitute every `Macro { text, .. }` whose name is in
/// `table` with the corresponding literal node. Spans on substituted
/// nodes are inherited from the original `Macro` node so diagnostics
/// keep pointing at the source.
fn fold_expr(expr: &ExprAst<Span>, table: &MacroTable) -> ExprAst<Span> {
    match expr {
        ExprAst::Integer { value, data } => ExprAst::Integer {
            value: *value,
            data: *data,
        },
        ExprAst::String { value, data } => ExprAst::String {
            value: value.clone(),
            data: *data,
        },
        ExprAst::Identifier { name, data } => ExprAst::Identifier {
            name: name.clone(),
            data: *data,
        },
        ExprAst::Macro { text, data } => fold_macro(text, *data, table),
        ExprAst::Paren { inner, data } => ExprAst::Paren {
            inner: Box::new(fold_expr(inner, table)),
            data: *data,
        },
        ExprAst::Not { inner, data } => ExprAst::Not {
            inner: Box::new(fold_expr(inner, table)),
            data: *data,
        },
        ExprAst::Binary {
            kind,
            lhs,
            rhs,
            data,
        } => ExprAst::Binary {
            kind: *kind,
            lhs: Box::new(fold_expr(lhs, table)),
            rhs: Box::new(fold_expr(rhs, table)),
            data: *data,
        },
        // `ExprAst` is `#[non_exhaustive]` — pass unknown variants
        // through. We err on the side of "don't fold what we can't model".
        other => other.clone(),
    }
}

fn fold_macro(text: &str, span: Span, table: &MacroTable) -> ExprAst<Span> {
    let Some((modifier, name)) = parse_macro_ref(text) else {
        return ExprAst::Macro {
            text: text.to_string(),
            data: span,
        };
    };
    let binding = table.get(&name);
    match (modifier, binding) {
        (MacroMod::None | MacroMod::IfDefined, Some(b)) => match &b.value {
            Some(MacroLiteral::Integer(i)) => ExprAst::Integer {
                value: *i,
                data: span,
            },
            Some(MacroLiteral::String(s)) => ExprAst::String {
                value: s.clone(),
                data: span,
            },
            None => ExprAst::Macro {
                text: text.to_string(),
                data: span,
            },
        },
        (MacroMod::IfNotDefined, Some(_)) => ExprAst::String {
            value: String::new(),
            data: span,
        },
        (MacroMod::IfDefined, None) => ExprAst::String {
            value: String::new(),
            data: span,
        },
        // Unconditional `%{X}` on an undefined macro — rpm would
        // expand to literal text "%{X}" or to nothing depending on
        // configuration. We can't reason about it; leave intact.
        (MacroMod::None, None) => ExprAst::Macro {
            text: text.to_string(),
            data: span,
        },
        // `%{!?X}` on undefined macro — expansion depends on the
        // optional `:default` form which we strip when parsing.
        // Conservative: leave intact.
        (MacroMod::IfNotDefined, None) => ExprAst::Macro {
            text: text.to_string(),
            data: span,
        },
    }
}

/// `true` when `expr` contains at least one `Macro` node (i.e. the
/// `%if` was originally macro-dependent; folding to a constant is
/// meaningful). Stops RPM117 from re-emitting RPM072's territory.
fn contains_macro_ref(expr: &ExprAst<Span>) -> bool {
    match expr {
        ExprAst::Macro { .. } => true,
        ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => contains_macro_ref(inner),
        ExprAst::Binary { lhs, rhs, .. } => contains_macro_ref(lhs) || contains_macro_ref(rhs),
        _ => false,
    }
}

// =====================================================================
// RPM117 — macro-defined-makes-if-trivial
// =====================================================================

pub static MACRO_FOLDS_IF_TRIVIAL_METADATA: LintMetadata = LintMetadata {
    id: "RPM117",
    name: "macro-defined-makes-if-trivial",
    description:
        "After substituting macro values defined earlier in the spec, the `%if` \
         expression reduces to a constant; the test is redundant.",
    // `%define FLAG <default>` followed by `%if %FLAG` is the idiomatic
    // **knob** pattern in real-world specs: the spec author declares a
    // default and lets `rpmbuild --define='FLAG <value>'` override it
    // at build time. The linter has no view of CLI overrides and would
    // flag every such knob as redundant — so we default to `Allow`.
    // Opt in via `--warn macro-defined-makes-if-trivial` for spec
    // hygiene passes where genuine dead constants matter.
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct MacroFoldsIfTrivial {
    diagnostics: Vec<Diagnostic>,
}

impl MacroFoldsIfTrivial {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MacroFoldsIfTrivial {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut table = MacroTable::default();
        walk_items_117(&spec.items, &mut table, &mut self.diagnostics);
    }
}

fn walk_items_117(
    items: &[SpecItem<Span>],
    table: &mut MacroTable,
    out: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            SpecItem::MacroDef(m) => table.insert(m),
            SpecItem::Conditional(c) => walk_conditional_117(c, table, out),
            SpecItem::Section(s) => walk_section_117(s, table, out),
            _ => {}
        }
    }
}

fn walk_conditional_117(
    cond: &Conditional<Span, SpecItem<Span>>,
    table: &mut MacroTable,
    out: &mut Vec<Diagnostic>,
) {
    for branch in &cond.branches {
        check_branch_117(&branch.expr, branch.data, table, out);
        let snap = table.clone();
        walk_items_117(&branch.body, table, out);
        *table = snap;
    }
    if let Some(els) = &cond.otherwise {
        let snap = table.clone();
        walk_items_117(els, table, out);
        *table = snap;
    }
}

fn check_branch_117(
    expr: &CondExpr<Span>,
    anchor: Span,
    table: &MacroTable,
    out: &mut Vec<Diagnostic>,
) {
    let CondExpr::Parsed(ast) = expr else { return };
    if !contains_macro_ref(ast) {
        return;
    }
    let folded = fold_expr(ast, table);
    let folded_cond = CondExpr::Parsed(Box::new(folded));
    let truthy = is_constant_true_condition(&folded_cond);
    let falsy = is_constant_false_condition(&folded_cond);
    if !truthy && !falsy {
        return;
    }
    let verdict = if truthy { "true" } else { "false" };
    out.push(
        Diagnostic::new(
            &MACRO_FOLDS_IF_TRIVIAL_METADATA,
            Severity::Warn,
            format!(
                "after substituting macro values, `%if` reduces to `{verdict}`; the test is redundant"
            ),
            anchor,
        )
        .with_suggestion(Suggestion::new(
            "drop the `%if` wrapper or simplify the expression",
            Vec::new(),
            Applicability::Manual,
        )),
    );
}

fn walk_section_117(s: &Section<Span>, table: &mut MacroTable, out: &mut Vec<Diagnostic>) {
    match s {
        Section::Package { content, .. } => {
            for c in content {
                if let PreambleContent::Conditional(cond) = c {
                    walk_preamble_conditional_117(cond, table, out);
                }
            }
        }
        Section::Files { content, .. } => {
            for c in content {
                if let FilesContent::Conditional(cond) = c {
                    walk_files_conditional_117(cond, table, out);
                }
            }
        }
        _ => {}
    }
}

fn walk_preamble_conditional_117(
    cond: &Conditional<Span, PreambleContent<Span>>,
    table: &mut MacroTable,
    out: &mut Vec<Diagnostic>,
) {
    for branch in &cond.branches {
        check_branch_117(&branch.expr, branch.data, table, out);
        // Preamble bodies don't define macros at the spec-item level,
        // so no snapshot/restore needed beyond the inner conditional
        // walk.
        for c in &branch.body {
            if let PreambleContent::Conditional(inner) = c {
                walk_preamble_conditional_117(inner, table, out);
            }
        }
    }
    if let Some(els) = &cond.otherwise {
        for c in els {
            if let PreambleContent::Conditional(inner) = c {
                walk_preamble_conditional_117(inner, table, out);
            }
        }
    }
}

fn walk_files_conditional_117(
    cond: &Conditional<Span, FilesContent<Span>>,
    table: &mut MacroTable,
    out: &mut Vec<Diagnostic>,
) {
    for branch in &cond.branches {
        check_branch_117(&branch.expr, branch.data, table, out);
        for c in &branch.body {
            if let FilesContent::Conditional(inner) = c {
                walk_files_conditional_117(inner, table, out);
            }
        }
    }
    if let Some(els) = &cond.otherwise {
        for c in els {
            if let FilesContent::Conditional(inner) = c {
                walk_files_conditional_117(inner, table, out);
            }
        }
    }
}

impl Lint for MacroFoldsIfTrivial {
    fn metadata(&self) -> &'static LintMetadata {
        &MACRO_FOLDS_IF_TRIVIAL_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM118 — unused-conditional-global
// =====================================================================

pub static UNUSED_CONDITIONAL_GLOBAL_METADATA: LintMetadata = LintMetadata {
    id: "RPM118",
    name: "unused-conditional-global",
    description:
        "`%global` macro is defined but never read elsewhere in the spec — may indicate \
         a leftover or unintended dead code.",
    // Defaults to `Allow` because `%global` is frequently used as a
    // public knob meant for downstream `rpmbuild --define` overrides,
    // and shell scripts may reference it via `${VAR}` which we don't
    // parse. Users who want this surfaced opt in via `--warn`.
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct UnusedConditionalGlobal {
    diagnostics: Vec<Diagnostic>,
    /// Set of names read anywhere in the spec via Text-segment macro
    /// references or ExprAst::Macro nodes.
    reads: std::collections::HashSet<String>,
    /// Definitions seen during the pass: `name → (span, is_global)`.
    /// Multiple definitions of the same name keep the latest span.
    defs: Vec<(String, Span)>,
}

impl UnusedConditionalGlobal {
    pub fn new() -> Self {
        Self::default()
    }

    fn record_def(&mut self, m: &MacroDef<Span>) {
        if matches!(m.kind, MacroDefKind::Global) {
            self.defs.push((m.name.clone(), m.data));
        }
    }

    fn record_text(&mut self, t: &Text) {
        for seg in &t.segments {
            if let TextSegment::Macro(mr) = seg {
                self.reads.insert(mr.name.clone());
                for a in &mr.args {
                    self.record_text(a);
                }
                if let Some(v) = &mr.with_value {
                    self.record_text(v);
                }
            }
        }
    }

    fn record_expr(&mut self, expr: &ExprAst<Span>) {
        match expr {
            ExprAst::Macro { text, .. } => {
                if let Some((_, name)) = parse_macro_ref(text) {
                    self.reads.insert(name);
                }
            }
            ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => self.record_expr(inner),
            ExprAst::Binary { lhs, rhs, .. } => {
                self.record_expr(lhs);
                self.record_expr(rhs);
            }
            // `ExprAst` is `#[non_exhaustive]`; literals and future
            // leaf variants carry no macro reads.
            _ => {}
        }
    }

    fn record_cond_expr(&mut self, c: &CondExpr<Span>) {
        match c {
            CondExpr::Parsed(ast) => self.record_expr(ast),
            CondExpr::Raw(t) => self.record_text(t),
            CondExpr::ArchList(items) => {
                for t in items {
                    self.record_text(t);
                }
            }
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for UnusedConditionalGlobal {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        walk_items_118(&spec.items, self);
        let read_names = std::mem::take(&mut self.reads);
        let defs = std::mem::take(&mut self.defs);
        for (name, span) in defs {
            if read_names.contains(&name) {
                continue;
            }
            self.diagnostics.push(
                Diagnostic::new(
                    &UNUSED_CONDITIONAL_GLOBAL_METADATA,
                    Severity::Warn,
                    format!("`%global {name}` is defined but never read"),
                    span,
                )
                .with_suggestion(Suggestion::new(
                    "remove the unused `%global` definition or reference it explicitly",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

fn walk_items_118(items: &[SpecItem<Span>], r: &mut UnusedConditionalGlobal) {
    for item in items {
        match item {
            SpecItem::MacroDef(m) => {
                r.record_def(m);
                r.record_text(&m.body);
            }
            SpecItem::Preamble(p) => walk_preamble_item_118(p, r),
            SpecItem::Conditional(c) => walk_top_cond_118(c, r),
            SpecItem::Section(s) => walk_section_118(s, r),
            _ => {}
        }
    }
}

fn walk_preamble_item_118(p: &PreambleItem<Span>, r: &mut UnusedConditionalGlobal) {
    use rpm_spec::ast::TagValue;
    match &p.value {
        TagValue::Text(t) => r.record_text(t),
        TagValue::ArchList(items) => {
            for t in items {
                r.record_text(t);
            }
        }
        // Dep/Bool/Number contain values that may include macro
        // references via inner Text fragments. Intentionally skipped:
        // covering DepExpr/EVR descent requires more surface area than
        // RPM118's read-detection needs to be useful in practice.
        _ => {}
    }
}

fn walk_top_cond_118(c: &Conditional<Span, SpecItem<Span>>, r: &mut UnusedConditionalGlobal) {
    for branch in &c.branches {
        r.record_cond_expr(&branch.expr);
        walk_items_118(&branch.body, r);
    }
    if let Some(els) = &c.otherwise {
        walk_items_118(els, r);
    }
}

fn walk_section_118(s: &Section<Span>, r: &mut UnusedConditionalGlobal) {
    match s {
        Section::Description { body, .. } => {
            for line in &body.lines {
                r.record_text(line);
            }
        }
        Section::Package { content, .. } => {
            for c in content {
                walk_preamble_content_118(c, r);
            }
        }
        Section::BuildScript { body, .. }
        | Section::Verify { body, .. }
        | Section::Sepolicy { body, .. } => {
            for line in &body.lines {
                r.record_text(line);
            }
        }
        Section::Files { file_lists, content, .. } => {
            for t in file_lists {
                r.record_text(t);
            }
            for fc in content {
                walk_files_content_118(fc, r);
            }
        }
        Section::Scriptlet(sl) => {
            for line in &sl.body.lines {
                r.record_text(line);
            }
        }
        Section::Trigger(t) => {
            for line in &t.body.lines {
                r.record_text(line);
            }
        }
        Section::FileTrigger(ft) => {
            for line in &ft.body.lines {
                r.record_text(line);
            }
        }
        Section::SourceList { entries, .. } | Section::PatchList { entries, .. } => {
            for t in entries {
                r.record_text(t);
            }
        }
        _ => {}
    }
}

fn walk_preamble_content_118(c: &PreambleContent<Span>, r: &mut UnusedConditionalGlobal) {
    match c {
        PreambleContent::Item(p) => walk_preamble_item_118(p, r),
        PreambleContent::Conditional(cond) => {
            for branch in &cond.branches {
                r.record_cond_expr(&branch.expr);
                for inner in &branch.body {
                    walk_preamble_content_118(inner, r);
                }
            }
            if let Some(els) = &cond.otherwise {
                for inner in els {
                    walk_preamble_content_118(inner, r);
                }
            }
        }
        _ => {}
    }
}

fn walk_files_content_118(c: &FilesContent<Span>, r: &mut UnusedConditionalGlobal) {
    match c {
        FilesContent::Entry(e) => {
            if let Some(p) = &e.path {
                r.record_text(&p.path);
            }
        }
        FilesContent::Conditional(cond) => {
            for branch in &cond.branches {
                r.record_cond_expr(&branch.expr);
                for inner in &branch.body {
                    walk_files_content_118(inner, r);
                }
            }
            if let Some(els) = &cond.otherwise {
                for inner in els {
                    walk_files_content_118(inner, r);
                }
            }
        }
        _ => {}
    }
}

impl Lint for UnusedConditionalGlobal {
    fn metadata(&self) -> &'static LintMetadata {
        &UNUSED_CONDITIONAL_GLOBAL_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- parse_macro_ref ----

    #[test]
    fn parse_macro_ref_handles_basic_forms() {
        assert_eq!(parse_macro_ref("%{X}"), Some((MacroMod::None, "X".to_string())));
        assert_eq!(parse_macro_ref("%{?X}"), Some((MacroMod::IfDefined, "X".to_string())));
        assert_eq!(
            parse_macro_ref("%{!?X}"),
            Some((MacroMod::IfNotDefined, "X".to_string()))
        );
        assert_eq!(parse_macro_ref("%X"), Some((MacroMod::None, "X".to_string())));
    }

    #[test]
    fn parse_macro_ref_strips_default_value() {
        assert_eq!(
            parse_macro_ref("%{?X:default}"),
            Some((MacroMod::IfDefined, "X".to_string()))
        );
    }

    #[test]
    fn parse_macro_ref_rejects_special_forms() {
        assert_eq!(parse_macro_ref("%(echo hi)"), None);
        assert_eq!(parse_macro_ref("%[1+2]"), None);
        assert_eq!(parse_macro_ref("%%"), None);
        assert_eq!(parse_macro_ref("%1"), None);
        assert_eq!(parse_macro_ref("%*"), None);
    }

    // ---- MacroTable ----

    #[test]
    fn macro_table_stores_integer_value() {
        let outcome = parse("Name: x\n%global FOO 1\n");
        let mut t = MacroTable::default();
        for item in &outcome.spec.items {
            if let SpecItem::MacroDef(m) = item {
                t.insert(m);
            }
        }
        let b = t.get("FOO").expect("defined");
        assert!(matches!(b.value, Some(MacroLiteral::Integer(1))));
    }

    #[test]
    fn macro_table_handles_undefine() {
        let outcome = parse("Name: x\n%global FOO 1\n%undefine FOO\n");
        let mut t = MacroTable::default();
        for item in &outcome.spec.items {
            if let SpecItem::MacroDef(m) = item {
                t.insert(m);
            }
        }
        assert!(t.get("FOO").is_none());
    }

    #[test]
    fn macro_table_returns_none_for_nontrivial_body() {
        let outcome = parse("Name: x\n%global FOO %{BAR}\n");
        let mut t = MacroTable::default();
        for item in &outcome.spec.items {
            if let SpecItem::MacroDef(m) = item {
                t.insert(m);
            }
        }
        let b = t.get("FOO").expect("defined");
        assert!(b.value.is_none(), "expected nontrivial body to yield None");
    }

    // ---- fold_expr ----

    fn parse_if_expr(src: &str) -> ExprAst<Span> {
        let outcome = parse(&format!("Name: x\n%if {src}\nLicense: MIT\n%endif\n"));
        for item in &outcome.spec.items {
            if let SpecItem::Conditional(c) = item {
                if let CondExpr::Parsed(ast) = &c.branches[0].expr {
                    return *ast.clone();
                }
            }
        }
        panic!("no parsed conditional for {src:?}")
    }

    #[test]
    fn fold_replaces_known_integer_macro() {
        let mut t = MacroTable::default();
        t.map.insert(
            "X".to_string(),
            MacroBinding {
                value: Some(MacroLiteral::Integer(1)),
            },
        );
        let expr = parse_if_expr("%{X}");
        let folded = fold_expr(&expr, &t);
        match folded {
            ExprAst::Integer { value, .. } => assert_eq!(value, 1),
            other => panic!("expected Integer, got {other:?}"),
        }
    }

    #[test]
    fn fold_leaves_unknown_macro_intact() {
        let t = MacroTable::default();
        let expr = parse_if_expr("%{X}");
        let folded = fold_expr(&expr, &t);
        assert!(matches!(folded, ExprAst::Macro { .. }));
    }

    // ---- RPM117 ----

    #[test]
    fn rpm117_flags_trivial_after_define() {
        let src = "Name: x\n%global FOO 1\n%if %{FOO}\nLicense: MIT\n%endif\n";
        let diags = run(src, MacroFoldsIfTrivial::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM117");
    }

    #[test]
    fn rpm117_flags_trivial_negative() {
        let src = "Name: x\n%global FOO 0\n%if %{FOO}\nLicense: MIT\n%endif\n";
        let diags = run(src, MacroFoldsIfTrivial::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("false"));
    }

    #[test]
    fn rpm117_flags_trivial_question_mark_reference() {
        let src = "Name: x\n%global FOO 1\n%if %{?FOO}\nLicense: MIT\n%endif\n";
        let diags = run(src, MacroFoldsIfTrivial::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm117_silent_when_macro_undefined() {
        let src = "Name: x\n%if %{FOO}\nLicense: MIT\n%endif\n";
        assert!(run(src, MacroFoldsIfTrivial::new()).is_empty());
    }

    #[test]
    fn rpm117_silent_when_body_nontrivial() {
        let src = "Name: x\n%global FOO %{BAR}\n%if %{FOO}\nLicense: MIT\n%endif\n";
        assert!(run(src, MacroFoldsIfTrivial::new()).is_empty());
    }

    #[test]
    fn rpm117_silent_for_pure_literal() {
        // %if 1 is RPM072 territory — not RPM117 (no macro to fold).
        let src = "Name: x\n%if 1\nLicense: MIT\n%endif\n";
        assert!(run(src, MacroFoldsIfTrivial::new()).is_empty());
    }

    #[test]
    fn rpm117_silent_when_define_inside_else_branch() {
        // FOO defined inside `%else` doesn't leak; the trailing
        // `%if %{FOO}` cannot be folded → silent.
        let src = "Name: x\n%if A\n%else\n%global FOO 1\n%endif\n\
                   %if %{FOO}\nLicense: MIT\n%endif\n";
        assert!(run(src, MacroFoldsIfTrivial::new()).is_empty());
    }

    // ---- RPM118 ----

    #[test]
    fn rpm118_flags_unused_global() {
        let src = "Name: x\n%global FOO 1\n";
        let diags = run(src, UnusedConditionalGlobal::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM118");
    }

    #[test]
    fn rpm118_silent_for_referenced_in_if() {
        let src = "Name: x\n%global FOO 1\n%if %{FOO}\nLicense: MIT\n%endif\n";
        assert!(run(src, UnusedConditionalGlobal::new()).is_empty());
    }

    #[test]
    fn rpm118_silent_for_referenced_in_preamble_value() {
        let src = "Name: x\n%global FOO 1\nVersion: %{FOO}\n";
        assert!(run(src, UnusedConditionalGlobal::new()).is_empty());
    }

    #[test]
    fn rpm118_skips_define_kind() {
        let src = "Name: x\n%define FOO 1\n";
        assert!(run(src, UnusedConditionalGlobal::new()).is_empty());
    }
}
