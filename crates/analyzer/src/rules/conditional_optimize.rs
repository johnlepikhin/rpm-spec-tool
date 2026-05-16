//! Phase 7 conditional-optimisation lints.
//!
//! These rules look at a single `%if` block (or the head expression
//! literal) and identify shapes that can be mechanically simplified
//! into a smaller equivalent. Auto-fixes are **Manual** in v1: the
//! AST doesn't yet expose keyword-level spans (`%if`/`%else`/`%endif`
//! positions, expression byte range), so producing a byte-precise
//! `Edit` would be unsafe. Diagnostics still point at the offending
//! block and the message states the equivalent.
//!
//! Rules:
//! - RPM080 `nested-and-collapse` — `%if A %if B FOO %endif %endif` →
//!   `%if (A) && (B) FOO %endif`.
//! - RPM081 `empty-else-drop` — `%if X FOO %else %endif` → drop
//!   the empty `%else` clause.
//! - RPM082 `invert-empty-if-arch` — `%ifarch X %else FOO %endif` →
//!   `%ifnarch X FOO %endif`. Only for arch/os branch kinds.
//! - RPM085 `constant-tautology-in-expr` — `%if X || 1`, `%if X && 0`,
//!   and friends. The expression has a constant operand that fixes
//!   the result.
//! - RPM087 `double-negation-in-expr` — `%if !!X` → `%if X`.

use rpm_spec::ast::{
    CondBranch, CondExpr, CondKind, Conditional, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

// =====================================================================
// RPM080 nested-and-collapse
// =====================================================================

pub static NESTED_AND_METADATA: LintMetadata = LintMetadata {
    id: "RPM080",
    name: "nested-and-collapse",
    description: "Two single-branch `%if` blocks nested directly can be merged into one with `&&`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct NestedAndCollapse {
    diagnostics: Vec<Diagnostic>,
}

impl NestedAndCollapse {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Returns the inner `Conditional` if `body` is a single-branch
/// (`%if`, not `%ifarch`/`%ifos`) block with no `%else`, exactly one
/// non-filler item that is itself a single-branch plain `%if` with no
/// `%else`. Filler = `Blank` / `Comment`.
fn nested_and_pattern_top(
    outer: &Conditional<Span, SpecItem<Span>>,
) -> Option<&Conditional<Span, SpecItem<Span>>> {
    if outer.branches.len() != 1 || outer.otherwise.is_some() {
        return None;
    }
    let outer_branch = &outer.branches[0];
    if !is_plain_if(outer_branch.kind) {
        return None;
    }
    let mut non_filler = outer_branch
        .body
        .iter()
        .filter(|i| !matches!(i, SpecItem::Blank | SpecItem::Comment(_)));
    let SpecItem::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if inner.branches.len() != 1 || inner.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(inner.branches[0].kind) {
        return None;
    }
    if !inner_branch_safe(inner) {
        return None;
    }
    Some(inner)
}

fn nested_and_pattern_preamble(
    outer: &Conditional<Span, PreambleContent<Span>>,
) -> Option<&Conditional<Span, PreambleContent<Span>>> {
    if outer.branches.len() != 1 || outer.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(outer.branches[0].kind) {
        return None;
    }
    let mut non_filler = outer.branches[0]
        .body
        .iter()
        .filter(|i| !matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)));
    let PreambleContent::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if inner.branches.len() != 1 || inner.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(inner.branches[0].kind) {
        return None;
    }
    if !inner_branch_safe(inner) {
        return None;
    }
    Some(inner)
}

fn nested_and_pattern_files(
    outer: &Conditional<Span, FilesContent<Span>>,
) -> Option<&Conditional<Span, FilesContent<Span>>> {
    if outer.branches.len() != 1 || outer.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(outer.branches[0].kind) {
        return None;
    }
    let mut non_filler = outer.branches[0]
        .body
        .iter()
        .filter(|i| !matches!(i, FilesContent::Blank | FilesContent::Comment(_)));
    let FilesContent::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if inner.branches.len() != 1 || inner.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(inner.branches[0].kind) {
        return None;
    }
    if !inner_branch_safe(inner) {
        return None;
    }
    Some(inner)
}

/// Plain `%if` (not arch/os-specialised). Arch/os conditions don't
/// compose with `&&` in RPM's expression grammar, so we don't suggest
/// merging them.
fn is_plain_if(kind: CondKind) -> bool {
    matches!(kind, CondKind::If)
}

/// `true` when the *inner* `%if`'s expression is safe to evaluate
/// unconditionally — i.e. merging it with the outer guard via `&&`
/// won't change semantics on systems where the outer evaluates to
/// false.
///
/// RPM evaluates `&&` eagerly (no short-circuit), so an unconditional
/// `%{name}` reference in the inner condition becomes a parse error
/// the moment the outer is false but the macro is undefined. Allowed
/// forms:
/// - `%{?name}` / `%{!?name}` / `%{?name:default}` — conditional refs,
///   expand to empty when undefined;
/// - `%%` — escaped literal `%`;
/// - no `%`-form at all.
///
/// Anything else (bare `%name` or unconditional `%{name}`) is risky
/// — until the profile system can vouch for the macro's availability,
/// we skip the suggestion.
fn inner_expr_safe_to_merge(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            // `%%` — escaped percent; harmless.
            b'%' => i += 2,
            // `%{...}` — peek past whitespace to the first
            // significant byte. Conditional sigils (`?`, `!`) are
            // safe; anything else means an unconditional reference.
            b'{' => {
                let mut j = i + 2;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                match bytes.get(j) {
                    Some(b'?' | b'!') => i = j + 1,
                    Some(_) => return false,
                    None => return false, // malformed; bail
                }
            }
            // `%name` (or `%(...)`, `%[...]`) — bare macro / shell /
            // expression form. Unconditional.
            _ => return false,
        }
    }
    true
}

/// `true` if the inner `%if`'s expression passes the safety check.
/// Wraps the literal extraction so the three context-specific
/// pattern detectors share one entry point.
fn inner_branch_safe<B>(inner: &Conditional<Span, B>) -> bool {
    let Some(first) = inner.branches.first() else {
        return false;
    };
    match &first.expr {
        CondExpr::Raw(text) => {
            let Some(lit) = text.literal_str() else {
                return false;
            };
            inner_expr_safe_to_merge(lit)
        }
        CondExpr::Parsed(ast) => expr_ast_safe_to_merge(ast),
        _ => false,
    }
}

/// Walk a parsed expression and verify every `%`-bearing leaf uses a
/// conditional macro reference (`%{?foo}`/`%{!?foo}`) or is otherwise
/// macro-free. Mirrors [`inner_expr_safe_to_merge`] but operates on
/// the AST directly.
///
/// String literals get scanned for embedded macros too — RPM expands
/// `"%{foo}"` at evaluation time, so an unconditional reference inside
/// quotes is just as dangerous as outside.
fn expr_ast_safe_to_merge<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast {
        ExprAst::Integer { .. } | ExprAst::Identifier { .. } => true,
        ExprAst::String { value, .. } => inner_expr_safe_to_merge(value),
        ExprAst::Macro { text, .. } => inner_expr_safe_to_merge(text),
        ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => expr_ast_safe_to_merge(inner),
        ExprAst::Binary { lhs, rhs, .. } => {
            expr_ast_safe_to_merge(lhs) && expr_ast_safe_to_merge(rhs)
        }
        _ => false,
    }
}

impl NestedAndCollapse {
    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &NESTED_AND_METADATA,
                Severity::Warn,
                "nested `%if`s can be safely merged into one with `&&` \
                 (inner condition uses only conditional macro refs)",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "rewrite as `%if (outer-expr) && (inner-expr)` and drop one `%if`/`%endif` pair",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for NestedAndCollapse {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        if nested_and_pattern_top(node).is_some() {
            self.emit(node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        if nested_and_pattern_preamble(node).is_some() {
            self.emit(node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        if nested_and_pattern_files(node).is_some() {
            self.emit(node.data);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for NestedAndCollapse {
    fn metadata(&self) -> &'static LintMetadata {
        &NESTED_AND_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM081 empty-else-drop
// =====================================================================

pub static EMPTY_ELSE_METADATA: LintMetadata = LintMetadata {
    id: "RPM081",
    name: "empty-else-drop",
    description: "`%else` clause has no content; drop the empty `%else`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct EmptyElseDrop {
    diagnostics: Vec<Diagnostic>,
}

impl EmptyElseDrop {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &EMPTY_ELSE_METADATA,
                Severity::Warn,
                "`%else` clause is empty (only blanks/comments) — drop it",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "delete the `%else` line and its empty body",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for EmptyElseDrop {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        // Fire only when at least one branch has real content,
        // otherwise RPM073 (empty-conditional-branch) handles the
        // whole block.
        let has_real_branch = node.branches.iter().any(|b| {
            !b.body
                .iter()
                .all(|i| matches!(i, SpecItem::Blank | SpecItem::Comment(_)))
        });
        if has_real_branch
            && let Some(other) = &node.otherwise
            && other
                .iter()
                .all(|i| matches!(i, SpecItem::Blank | SpecItem::Comment(_)))
        {
            self.emit(node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        let has_real_branch = node.branches.iter().any(|b| {
            !b.body
                .iter()
                .all(|i| matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)))
        });
        if has_real_branch
            && let Some(other) = &node.otherwise
            && other
                .iter()
                .all(|i| matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)))
        {
            self.emit(node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        let has_real_branch = node.branches.iter().any(|b| {
            !b.body
                .iter()
                .all(|i| matches!(i, FilesContent::Blank | FilesContent::Comment(_)))
        });
        if has_real_branch
            && let Some(other) = &node.otherwise
            && other
                .iter()
                .all(|i| matches!(i, FilesContent::Blank | FilesContent::Comment(_)))
        {
            self.emit(node.data);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for EmptyElseDrop {
    fn metadata(&self) -> &'static LintMetadata {
        &EMPTY_ELSE_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM082 invert-empty-if-arch
// =====================================================================

pub static INVERT_EMPTY_IF_ARCH_METADATA: LintMetadata = LintMetadata {
    id: "RPM082",
    name: "invert-empty-if-arch",
    description: "`%ifarch X %else FOO %endif` — empty `%if` branch with content in `%else`; flip kind.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct InvertEmptyIfArch {
    diagnostics: Vec<Diagnostic>,
}

impl InvertEmptyIfArch {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span, hint_kind: &'static str) {
        self.diagnostics.push(
            Diagnostic::new(
                &INVERT_EMPTY_IF_ARCH_METADATA,
                Severity::Warn,
                format!("flip `%{hint_kind}` to its negation and drop the empty branch"),
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "rewrite to use the opposite arch/os keyword without `%else`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// `Some(opposite_keyword)` when `kind` is one of the arch/os branch
/// kinds we can flip.
fn flippable_arch_kind(kind: CondKind) -> Option<&'static str> {
    match kind {
        CondKind::IfArch => Some("ifnarch"),
        CondKind::IfNArch => Some("ifarch"),
        CondKind::IfOs => Some("ifnos"),
        CondKind::IfNOs => Some("ifos"),
        _ => None,
    }
}

fn check_invert_empty<B>(
    node: &Conditional<Span, B>,
    branch_filler: impl Fn(&B) -> bool,
    branch_real: impl Fn(&B) -> bool,
) -> Option<&'static str> {
    // Must be exactly one branch + a non-empty `%else`.
    if node.branches.len() != 1 {
        return None;
    }
    let other = node.otherwise.as_ref()?;
    let head = &node.branches[0];
    let hint = flippable_arch_kind(head.kind)?;
    // `%if` body must be empty (only blanks/comments).
    if !head.body.iter().all(&branch_filler) {
        return None;
    }
    // `%else` body must have real content.
    if !other.iter().any(&branch_real) {
        return None;
    }
    Some(hint)
}

impl<'ast> Visit<'ast> for InvertEmptyIfArch {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        if let Some(hint) = check_invert_empty(
            node,
            |i| matches!(i, SpecItem::Blank | SpecItem::Comment(_)),
            |i| !matches!(i, SpecItem::Blank | SpecItem::Comment(_)),
        ) {
            self.emit(node.data, hint);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        if let Some(hint) = check_invert_empty(
            node,
            |i| matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)),
            |i| !matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)),
        ) {
            self.emit(node.data, hint);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        if let Some(hint) = check_invert_empty(
            node,
            |i| matches!(i, FilesContent::Blank | FilesContent::Comment(_)),
            |i| !matches!(i, FilesContent::Blank | FilesContent::Comment(_)),
        ) {
            self.emit(node.data, hint);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for InvertEmptyIfArch {
    fn metadata(&self) -> &'static LintMetadata {
        &INVERT_EMPTY_IF_ARCH_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// Expression-level lints (RPM085, RPM087)
// =====================================================================
//
// Both inspect the literal text of `%if`/`%elif` expressions. The
// parser stores the expression as a single literal string when no
// macro segmentation is needed — `0%{?rhel}` ends up as one
// `Literal("0%{?rhel}")` — so a `%` in the string means there's a
// macro reference *somewhere* and we bail out conservatively.

/// View of an `%if` expression in one of the two parser-produced
/// forms. Rules that scan operators / operands at literal-text level
/// (RPM085, RPM087) work on `Raw`; structural rules use `Parsed`.
enum BranchExprView<'a, T> {
    Raw(&'a str),
    Parsed(&'a rpm_spec::ast::ExprAst<T>),
}

fn each_branch_expr<B>(
    node: &Conditional<Span, B>,
) -> impl Iterator<Item = (&CondBranch<Span, B>, BranchExprView<'_, Span>)> {
    node.branches.iter().filter_map(|b| match &b.expr {
        CondExpr::Raw(t) => t.literal_str().map(|s| (b, BranchExprView::Raw(s))),
        CondExpr::Parsed(ast) => Some((b, BranchExprView::Parsed(ast.as_ref()))),
        _ => None,
    })
}

// ---- RPM085 constant-tautology-in-expr ----

pub static CONSTANT_TAUTOLOGY_METADATA: LintMetadata = LintMetadata {
    id: "RPM085",
    name: "constant-tautology-in-expr",
    description: "Expression contains a constant operand (`|| 1`, `&& 0`, …) that fixes the result.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct ConstantTautologyInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl ConstantTautologyInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Detect the simple constant-tautology patterns at top-level of the
/// expression. Returns a short label for the matched pattern so the
/// diagnostic can quote the offending fragment. Conservative: bails
/// on any `%` (macro presence) because the parser doesn't tokenise
/// the expression grammar.
fn detect_tautology(expr: &str) -> Option<&'static str> {
    let trimmed = expr.trim();
    if trimmed.contains('%') {
        return None;
    }
    // Normalise: `true` → `1`, `false` → `0`, drop whitespace. We
    // never rewrite the source, only the buffer we scan.
    let norm: String = trimmed
        .replace("true", "1")
        .replace("false", "0")
        .replace(char::is_whitespace, "");
    let bytes = norm.as_bytes();
    // Pattern "||1": reject if a digit follows the `1` (else `||10`
    // would match). Before the `||` anything is fine.
    if let Some(idx) = norm.find("||1") {
        let after = bytes.get(idx + 3).copied();
        if !matches!(after, Some(c) if c.is_ascii_digit()) {
            return Some("always-true (`|| 1`)");
        }
    }
    // Pattern "1||": reject if a digit precedes the `1`.
    if let Some(idx) = norm.find("1||") {
        let before = if idx == 0 {
            None
        } else {
            bytes.get(idx - 1).copied()
        };
        if !matches!(before, Some(c) if c.is_ascii_digit()) {
            return Some("always-true (`1 ||`)");
        }
    }
    // Pattern "&&0": reject if a digit follows the `0`.
    if let Some(idx) = norm.find("&&0") {
        let after = bytes.get(idx + 3).copied();
        if !matches!(after, Some(c) if c.is_ascii_digit()) {
            return Some("always-false (`&& 0`)");
        }
    }
    // Pattern "0&&": reject if a digit precedes the `0`.
    if let Some(idx) = norm.find("0&&") {
        let before = if idx == 0 {
            None
        } else {
            bytes.get(idx - 1).copied()
        };
        if !matches!(before, Some(c) if c.is_ascii_digit()) {
            return Some("always-false (`0 &&`)");
        }
    }
    None
}

impl ConstantTautologyInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for (branch, view) in each_branch_expr(node) {
            let label = match view {
                BranchExprView::Raw(s) => detect_tautology(s),
                BranchExprView::Parsed(ast) => detect_tautology_ast(ast),
            };
            if let Some(label) = label {
                self.diagnostics.push(
                    Diagnostic::new(
                        &CONSTANT_TAUTOLOGY_METADATA,
                        Severity::Warn,
                        format!("condition is {label}; simplify or drop the guard"),
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the constant operand and re-check the rest of the expression",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

/// AST-based tautology detection mirroring [`detect_tautology`] but
/// walking the structured tree. Catches `Binary(LogOr, c, _)` where
/// `c` is constant-true (or symmetrically on the other side) and
/// `Binary(LogAnd, c, _)` where `c` is constant-false.
fn detect_tautology_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> Option<&'static str> {
    use rpm_spec::ast::{BinOp, ExprAst};
    match ast.peel_parens() {
        ExprAst::Binary {
            kind: BinOp::LogOr,
            lhs,
            rhs,
            ..
        } => {
            if is_const_true_ast(lhs) || is_const_true_ast(rhs) {
                return Some("always-true (`|| 1`)");
            }
            // Recurse into children to catch nested tautologies.
            detect_tautology_ast(lhs).or_else(|| detect_tautology_ast(rhs))
        }
        ExprAst::Binary {
            kind: BinOp::LogAnd,
            lhs,
            rhs,
            ..
        } => {
            if is_const_false_ast(lhs) || is_const_false_ast(rhs) {
                return Some("always-false (`&& 0`)");
            }
            detect_tautology_ast(lhs).or_else(|| detect_tautology_ast(rhs))
        }
        ExprAst::Not { inner, .. } => detect_tautology_ast(inner),
        _ => None,
    }
}

fn is_const_true_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast.peel_parens() {
        ExprAst::Integer { value, .. } => *value != 0,
        ExprAst::String { value, .. } => !value.is_empty(),
        ExprAst::Identifier { name, .. } => name == "true",
        _ => false,
    }
}

fn is_const_false_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast.peel_parens() {
        ExprAst::Integer { value, .. } => *value == 0,
        ExprAst::String { value, .. } => value.is_empty(),
        ExprAst::Identifier { name, .. } => name == "false",
        _ => false,
    }
}

impl<'ast> Visit<'ast> for ConstantTautologyInExpr {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for ConstantTautologyInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &CONSTANT_TAUTOLOGY_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// ---- RPM087 double-negation-in-expr ----

pub static DOUBLE_NEGATION_METADATA: LintMetadata = LintMetadata {
    id: "RPM087",
    name: "double-negation-in-expr",
    description: "Double negation (`!!`) in `%if` expression — drop it.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct DoubleNegationInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl DoubleNegationInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `true` if `expr` contains a `!!` token (two consecutive `!`s) in
/// its literal form. We don't try to handle `! !` with a space —
/// that's a different shape and would need real tokenisation.
fn has_double_negation(expr: &str) -> bool {
    expr.contains("!!")
}

/// AST-based double-negation: any `Not { inner: Not { .. } }` subtree
/// counts.
fn has_double_negation_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast {
        ExprAst::Not { inner, .. } => {
            matches!(inner.as_ref().peel_parens(), ExprAst::Not { .. })
                || has_double_negation_ast(inner)
        }
        ExprAst::Paren { inner, .. } => has_double_negation_ast(inner),
        ExprAst::Binary { lhs, rhs, .. } => {
            has_double_negation_ast(lhs) || has_double_negation_ast(rhs)
        }
        _ => false,
    }
}

impl DoubleNegationInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for (branch, view) in each_branch_expr(node) {
            let hit = match view {
                BranchExprView::Raw(s) => has_double_negation(s),
                BranchExprView::Parsed(ast) => has_double_negation_ast(ast),
            };
            if hit {
                self.diagnostics.push(
                    Diagnostic::new(
                        &DOUBLE_NEGATION_METADATA,
                        Severity::Warn,
                        "double negation `!!` is redundant — drop it",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the two `!` characters",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for DoubleNegationInExpr {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for DoubleNegationInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &DOUBLE_NEGATION_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM083 collapse-elif-into-else (Phase 7b)
// =====================================================================

pub static COLLAPSE_ELIF_METADATA: LintMetadata = LintMetadata {
    id: "RPM083",
    name: "collapse-elif-into-else",
    description: "Final `%elif` with a constant-true expression is equivalent to `%else`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct CollapseElifIntoElse {
    diagnostics: Vec<Diagnostic>,
}

impl CollapseElifIntoElse {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        // Need at least one `%elif` (i.e. branches.len() >= 2). And
        // no `%else` already — otherwise the final `%elif true` is
        // either redundant or covered by other rules.
        if node.branches.len() < 2 || node.otherwise.is_some() {
            return;
        }
        let Some(last) = node.branches.last() else {
            return;
        };
        if !matches!(last.kind, CondKind::Elif) {
            return;
        }
        if !crate::rules::util::is_constant_true_condition(&last.expr) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &COLLAPSE_ELIF_METADATA,
                Severity::Warn,
                "final `%elif` with constant-true condition can become `%else`",
                last.data,
            )
            .with_suggestion(Suggestion::new(
                "replace the `%elif <true>` keyword line with `%else`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for CollapseElifIntoElse {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for CollapseElifIntoElse {
    fn metadata(&self) -> &'static LintMetadata {
        &COLLAPSE_ELIF_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM086 idempotent-in-expr (Phase 7b)
// =====================================================================

pub static IDEMPOTENT_METADATA: LintMetadata = LintMetadata {
    id: "RPM086",
    name: "idempotent-in-expr",
    description: "`X && X` / `X || X` repeats an operand — drop the duplicate.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct IdempotentInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl IdempotentInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

fn find_idempotent_op<T>(ast: &rpm_spec::ast::ExprAst<T>) -> Option<&rpm_spec::ast::ExprAst<T>> {
    use rpm_spec::ast::{BinOp, ExprAst};
    match ast.peel_parens() {
        ExprAst::Binary {
            kind: BinOp::LogAnd | BinOp::LogOr,
            lhs,
            rhs,
            ..
        } => {
            if exprs_equiv(lhs, rhs) {
                return Some(ast);
            }
            find_idempotent_op(lhs).or_else(|| find_idempotent_op(rhs))
        }
        ExprAst::Not { inner, .. } | ExprAst::Paren { inner, .. } => find_idempotent_op(inner),
        ExprAst::Binary { lhs, rhs, .. } => {
            find_idempotent_op(lhs).or_else(|| find_idempotent_op(rhs))
        }
        _ => None,
    }
}

// `exprs_equiv` has been promoted to `crate::rules::util` for reuse
// across Phase 7 rules. Local duplicate removed.
use crate::rules::util::exprs_equiv;

impl IdempotentInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if find_idempotent_op(ast).is_some() {
                self.diagnostics.push(
                    Diagnostic::new(
                        &IDEMPOTENT_METADATA,
                        Severity::Warn,
                        "`X && X` / `X || X` repeats an operand — simplify",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the duplicated operand",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for IdempotentInExpr {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for IdempotentInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &IDEMPOTENT_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM088 self-comparison-in-expr (Phase 7b)
// =====================================================================

pub static SELF_COMPARISON_METADATA: LintMetadata = LintMetadata {
    id: "RPM088",
    name: "self-comparison-in-expr",
    description: "Comparison of an operand with itself has a fixed outcome.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct SelfComparisonInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl SelfComparisonInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

fn find_self_comparison<T>(ast: &rpm_spec::ast::ExprAst<T>) -> Option<&'static str> {
    use rpm_spec::ast::{BinOp, ExprAst};
    match ast.peel_parens() {
        ExprAst::Binary { kind, lhs, rhs, .. } => {
            let cmp = matches!(
                kind,
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
            );
            if cmp && exprs_equiv(lhs, rhs) {
                let verdict = match kind {
                    BinOp::Eq | BinOp::Le | BinOp::Ge => "always-true",
                    BinOp::Ne | BinOp::Lt | BinOp::Gt => "always-false",
                    _ => "always-constant",
                };
                return Some(verdict);
            }
            find_self_comparison(lhs).or_else(|| find_self_comparison(rhs))
        }
        ExprAst::Not { inner, .. } | ExprAst::Paren { inner, .. } => find_self_comparison(inner),
        _ => None,
    }
}

impl SelfComparisonInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if let Some(verdict) = find_self_comparison(ast) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &SELF_COMPARISON_METADATA,
                        Severity::Warn,
                        format!("comparison of an operand with itself is {verdict}"),
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "replace the redundant comparison with the constant outcome",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for SelfComparisonInExpr {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for SelfComparisonInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &SELF_COMPARISON_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM094 line-continuation-in-condition (Phase 7b)
// =====================================================================

pub static LINE_CONT_METADATA: LintMetadata = LintMetadata {
    id: "RPM094",
    name: "line-continuation-in-condition",
    description: "`%if` expression spans multiple lines via `\\` — RPM doesn't support continuation here.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct LineContinuationInCondition {
    diagnostics: Vec<Diagnostic>,
}

impl LineContinuationInCondition {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Raw(text) = &branch.expr else {
                continue;
            };
            let Some(lit) = text.literal_str() else {
                continue;
            };
            // `logical_line` joins continuation lines with a literal
            // `\n` separator — a `\n` mid-expression is the signal
            // that the author tried to split `%if` across lines.
            if lit.contains('\n') {
                self.diagnostics.push(Diagnostic::new(
                    &LINE_CONT_METADATA,
                    Severity::Warn,
                    "`%if` expression continues onto another line — \
                     RPM does not honour `\\` continuation in conditions",
                    branch.data,
                ));
            }
        }
    }
}

impl<'ast> Visit<'ast> for LineContinuationInCondition {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for LineContinuationInCondition {
    fn metadata(&self) -> &'static LintMetadata {
        &LINE_CONT_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM100 collapse-else-if-into-elif (Phase 7c)
// =====================================================================

pub static COLLAPSE_ELSE_IF_METADATA: LintMetadata = LintMetadata {
    id: "RPM100",
    name: "collapse-else-if-into-elif",
    description: "`%else` containing a single `%if` block can be folded into an `%elif` — \
         drops one nesting level.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct CollapseElseIfIntoElif {
    diagnostics: Vec<Diagnostic>,
}

impl CollapseElseIfIntoElif {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &COLLAPSE_ELSE_IF_METADATA,
                Severity::Warn,
                "`%else` contains a single nested `%if` — collapse into `%elif`",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "rewrite the `%else` + nested `%if` as `%elif <inner-cond>`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// `Some(inner_span)` when `body` (the `%else` arm) holds exactly one
/// real item and that item is a `%if` block whose head is plain
/// `CondKind::If`. The inner may itself have `%elif` arms and/or an
/// `%else` — in those cases the whole inner chain (including its own
/// trailing `%else`) merges cleanly into the outer chain via
/// `%elif <inner-cond>`. Filler items (Blank/Comment) are tolerated.
fn else_holds_single_if_top(body: &[SpecItem<Span>]) -> Option<Span> {
    let mut non_filler = body
        .iter()
        .filter(|i| !matches!(i, SpecItem::Blank | SpecItem::Comment(_)));
    let SpecItem::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if !matches!(inner.branches.first()?.kind, CondKind::If) {
        return None;
    }
    Some(inner.data)
}

fn else_holds_single_if_preamble(body: &[PreambleContent<Span>]) -> Option<Span> {
    let mut non_filler = body
        .iter()
        .filter(|i| !matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)));
    let PreambleContent::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if !matches!(inner.branches.first()?.kind, CondKind::If) {
        return None;
    }
    Some(inner.data)
}

fn else_holds_single_if_files(body: &[FilesContent<Span>]) -> Option<Span> {
    let mut non_filler = body
        .iter()
        .filter(|i| !matches!(i, FilesContent::Blank | FilesContent::Comment(_)));
    let FilesContent::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if !matches!(inner.branches.first()?.kind, CondKind::If) {
        return None;
    }
    Some(inner.data)
}

impl<'ast> Visit<'ast> for CollapseElseIfIntoElif {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        if let Some(body) = &node.otherwise
            && else_holds_single_if_top(body).is_some()
        {
            self.emit(node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        if let Some(body) = &node.otherwise
            && else_holds_single_if_preamble(body).is_some()
        {
            self.emit(node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        if let Some(body) = &node.otherwise
            && else_holds_single_if_files(body).is_some()
        {
            self.emit(node.data);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for CollapseElseIfIntoElif {
    fn metadata(&self) -> &'static LintMetadata {
        &COLLAPSE_ELSE_IF_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM101 absorption-in-expr (Phase 7c)
// =====================================================================

pub static ABSORPTION_METADATA: LintMetadata = LintMetadata {
    id: "RPM101",
    name: "absorption-in-expr",
    description: "Boolean absorption: `A || (A && B)` reduces to `A`; `A && (A || B)` reduces to `A`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct AbsorptionInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl AbsorptionInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Detect any absorption pattern anywhere in the AST:
/// - `A || (A && B)` / `(A && B) || A`
/// - `A && (A || B)` / `(A || B) && A`
fn has_absorption<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::{BinOp, ExprAst};
    let bare = ast.peel_parens();
    if let ExprAst::Binary { kind, lhs, rhs, .. } = bare {
        let lhs_inner = lhs.as_ref().peel_parens();
        let rhs_inner = rhs.as_ref().peel_parens();
        match kind {
            BinOp::LogOr => {
                // A || (A && B)
                if let ExprAst::Binary {
                    kind: BinOp::LogAnd,
                    lhs: l2,
                    rhs: r2,
                    ..
                } = rhs_inner
                    && (exprs_equiv(lhs, l2) || exprs_equiv(lhs, r2))
                {
                    return true;
                }
                if let ExprAst::Binary {
                    kind: BinOp::LogAnd,
                    lhs: l2,
                    rhs: r2,
                    ..
                } = lhs_inner
                    && (exprs_equiv(rhs, l2) || exprs_equiv(rhs, r2))
                {
                    return true;
                }
            }
            BinOp::LogAnd => {
                // A && (A || B)
                if let ExprAst::Binary {
                    kind: BinOp::LogOr,
                    lhs: l2,
                    rhs: r2,
                    ..
                } = rhs_inner
                    && (exprs_equiv(lhs, l2) || exprs_equiv(lhs, r2))
                {
                    return true;
                }
                if let ExprAst::Binary {
                    kind: BinOp::LogOr,
                    lhs: l2,
                    rhs: r2,
                    ..
                } = lhs_inner
                    && (exprs_equiv(rhs, l2) || exprs_equiv(rhs, r2))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    // Recurse into sub-expressions.
    match bare {
        ExprAst::Binary { lhs, rhs, .. } => has_absorption(lhs) || has_absorption(rhs),
        ExprAst::Not { inner, .. } => has_absorption(inner),
        _ => false,
    }
}

impl AbsorptionInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if has_absorption(ast) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &ABSORPTION_METADATA,
                        Severity::Warn,
                        "boolean absorption: simplify `A || (A && B)` → `A` \
                         (or `A && (A || B)` → `A`)",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the absorbed sub-expression",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for AbsorptionInExpr {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for AbsorptionInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &ABSORPTION_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM104 string-set-redundancy (Phase 7d)
// =====================================================================

pub static STRING_SET_METADATA: LintMetadata = LintMetadata {
    id: "RPM104",
    name: "string-set-redundancy",
    description: "`X == \"a\" || X == \"a\"` repeats the same string in an `||`-chain — drop the duplicate.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct StringSetRedundancy {
    diagnostics: Vec<Diagnostic>,
}

impl StringSetRedundancy {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Walk the top-level `||`-chain of `ast` and accumulate operands.
fn flatten_or_chain<'a, T>(
    ast: &'a rpm_spec::ast::ExprAst<T>,
    out: &mut Vec<&'a rpm_spec::ast::ExprAst<T>>,
) {
    use rpm_spec::ast::{BinOp, ExprAst};
    match ast.peel_parens() {
        ExprAst::Binary {
            kind: BinOp::LogOr,
            lhs,
            rhs,
            ..
        } => {
            flatten_or_chain(lhs, out);
            flatten_or_chain(rhs, out);
        }
        other => out.push(other),
    }
}

/// `Some((lhs, string_value))` when `ast` is `LHS == "literal"`.
/// Any other shape (different op, non-string rhs) returns `None`.
fn extract_string_eq<T>(
    ast: &rpm_spec::ast::ExprAst<T>,
) -> Option<(&rpm_spec::ast::ExprAst<T>, &str)> {
    use rpm_spec::ast::{BinOp, ExprAst};
    if let ExprAst::Binary {
        kind: BinOp::Eq,
        lhs,
        rhs,
        ..
    } = ast.peel_parens()
        && let ExprAst::String { value, .. } = rhs.peel_parens()
    {
        return Some((lhs, value));
    }
    None
}

impl StringSetRedundancy {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            let mut operands = Vec::new();
            flatten_or_chain(ast, &mut operands);
            if operands.len() < 2 {
                continue;
            }
            let pairs: Vec<_> = operands
                .iter()
                .filter_map(|o| extract_string_eq(o))
                .collect();
            // Look for any pair (i, j) with same lhs and same string.
            let mut found_dup = false;
            for i in 0..pairs.len() {
                for j in (i + 1)..pairs.len() {
                    let (lhs1, s1) = pairs[i];
                    let (lhs2, s2) = pairs[j];
                    if s1 == s2 && exprs_equiv(lhs1, lhs2) {
                        found_dup = true;
                        break;
                    }
                }
                if found_dup {
                    break;
                }
            }
            if found_dup {
                self.diagnostics.push(
                    Diagnostic::new(
                        &STRING_SET_METADATA,
                        Severity::Warn,
                        "duplicate `X == \"...\"` operand in `||`-chain — drop the repeat",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the repeated equality comparison",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for StringSetRedundancy {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for StringSetRedundancy {
    fn metadata(&self) -> &'static LintMetadata {
        &STRING_SET_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM105 inverted-if-else (Phase 7d)
// =====================================================================

pub static INVERTED_IF_ELSE_METADATA: LintMetadata = LintMetadata {
    id: "RPM105",
    name: "inverted-if-else",
    description: "`%if !X foo %else bar %endif` reads more naturally when the negation is removed and \
         the branches are swapped.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct InvertedIfElse {
    diagnostics: Vec<Diagnostic>,
}

impl InvertedIfElse {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `true` when the head expression is `!something` — either a
/// parsed `Not` node or a `Raw` literal starting with `!` (and not
/// `!=`, which is the inequality operator).
fn head_is_negation<T>(expr: &CondExpr<T>) -> bool {
    match expr {
        CondExpr::Parsed(ast) => matches!(ast.peel_parens(), rpm_spec::ast::ExprAst::Not { .. }),
        CondExpr::Raw(text) => match text.literal_str() {
            Some(lit) => {
                let trimmed = lit.trim_start();
                trimmed.starts_with('!') && !trimmed.starts_with("!=")
            }
            None => false,
        },
        _ => false,
    }
}

impl InvertedIfElse {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        // Pattern: exactly one branch (no `%elif`), a non-empty `%else`,
        // and the `%if` head is `!X`.
        if node.branches.len() != 1 || node.otherwise.is_none() {
            return;
        }
        let branch = &node.branches[0];
        if !head_is_negation(&branch.expr) {
            return;
        }
        // Only plain `%if`; arch/os negations have dedicated keywords
        // (`%ifnarch`/`%ifnos`), so RPM082 handles those cases.
        if !matches!(branch.kind, CondKind::If) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &INVERTED_IF_ELSE_METADATA,
                Severity::Warn,
                "`%if !X ... %else ... %endif` — remove the negation and swap the branch bodies",
                node.data,
            )
            .with_suggestion(Suggestion::new(
                "drop the leading `!` and swap the `%if` body with the `%else` body",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for InvertedIfElse {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for InvertedIfElse {
    fn metadata(&self) -> &'static LintMetadata {
        &INVERTED_IF_ELSE_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM080 nested-and-collapse ----

    #[test]
    fn rpm080_flags_nested_and() {
        let src = "Name: x\n%if 1\n%if 1\nVersion: 1\n%endif\n%endif\n";
        let diags = run(src, NestedAndCollapse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM080");
    }

    #[test]
    fn rpm080_silent_when_outer_has_else() {
        let src = "Name: x\n%if 1\n%if 1\nVersion: 1\n%endif\n%else\nVersion: 2\n%endif\n";
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_silent_when_inner_has_else() {
        let src = "Name: x\n%if 1\n%if 1\nVersion: 1\n%else\nVersion: 2\n%endif\n%endif\n";
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_silent_when_extra_item_outside_inner() {
        let src = "Name: x\n%if 1\nLicense: MIT\n%if 1\nVersion: 1\n%endif\n%endif\n";
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_silent_for_ifarch_outer() {
        // `%ifarch` can't be merged with a plain `%if` via `&&`.
        let src = "Name: x\n%ifarch x86_64\n%if 1\nVersion: 1\n%endif\n%endif\n";
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_allows_blank_around_inner() {
        // Blank lines inside outer's body don't count as content.
        let src = "Name: x\n%if 1\n\n%if 1\nVersion: 1\n%endif\n\n%endif\n";
        let diags = run(src, NestedAndCollapse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm080_flags_conditional_macro_in_inner() {
        // Inner uses `%{?sle_version}` — conditional ref, safe to
        // merge: undefined → empty → `0 >= 1` → false.
        let src = "Name: x\n%if \"%{?_vendor}\" == \"suse\"\n\
                   %if 0%{?sle_version} >= 120000\nVersion: 1\n%endif\n%endif\n";
        let diags = run(src, NestedAndCollapse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm080_silent_for_unconditional_braced_macro_in_inner() {
        // Inner references `%{pgsql_major}` without `?`. On a system
        // where that macro is undefined, evaluating the merged
        // condition would parse-error. Skip the suggestion.
        let src = "Name: x\n%if 1\n%if %{pgsql_major} >= 17\nVersion: 1\n%endif\n%endif\n";
        assert!(
            run(src, NestedAndCollapse::new()).is_empty(),
            "should bail on unconditional %{{...}} in inner"
        );
    }

    #[test]
    fn rpm080_silent_for_bare_macro_in_inner() {
        // `%pgsql_major` bare form — same risk as `%{pgsql_major}`.
        let src = "Name: x\n%if 1\n%if %pgsql_major >= 17\nVersion: 1\n%endif\n%endif\n";
        assert!(
            run(src, NestedAndCollapse::new()).is_empty(),
            "should bail on bare %name in inner"
        );
    }

    #[test]
    fn rpm080_flags_double_percent_escape_in_inner() {
        // `%%` is an escaped literal `%`, not a macro reference.
        // Inner has no real macro refs — safe.
        let src = "Name: x\n%if 1\n%if \"%%foo\" == \"%foo\"\nVersion: 1\n%endif\n%endif\n";
        // NB: this is a deliberately weird condition; the point is
        // that `%%foo` should NOT count as an unconditional macro.
        // The `\"%foo\"` later WOULD count, but in this test we keep
        // only `%%foo`:
        let safer = "Name: x\n%if 1\n%if 1 == 1\nVersion: 1\n%endif\n%endif\n";
        assert_eq!(run(safer, NestedAndCollapse::new()).len(), 1);
        // The original (with bare `%foo`) is correctly rejected:
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_unit_inner_expr_safe_to_merge() {
        // Direct unit tests for the safety classifier.
        assert!(inner_expr_safe_to_merge("1"));
        assert!(inner_expr_safe_to_merge("1 || 0"));
        assert!(inner_expr_safe_to_merge("0%{?foo} >= 1"));
        assert!(inner_expr_safe_to_merge("%{!?foo:1}"));
        assert!(inner_expr_safe_to_merge("\"%%x\" == \"y\""));
        assert!(!inner_expr_safe_to_merge("%{foo}"));
        assert!(!inner_expr_safe_to_merge("%foo"));
        assert!(!inner_expr_safe_to_merge("%(echo hi)"));
        assert!(!inner_expr_safe_to_merge("0%{?ok} && %{bad}"));
    }

    // ---- RPM081 empty-else-drop ----

    #[test]
    fn rpm081_flags_empty_else() {
        let src = "Name: x\n%if 1\nVersion: 1\n%else\n%endif\n";
        let diags = run(src, EmptyElseDrop::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM081");
    }

    #[test]
    fn rpm081_silent_when_else_has_content() {
        let src = "Name: x\n%if 1\nVersion: 1\n%else\nVersion: 2\n%endif\n";
        assert!(run(src, EmptyElseDrop::new()).is_empty());
    }

    #[test]
    fn rpm081_silent_when_no_else() {
        let src = "Name: x\n%if 1\nVersion: 1\n%endif\n";
        assert!(run(src, EmptyElseDrop::new()).is_empty());
    }

    #[test]
    fn rpm081_silent_when_all_branches_empty() {
        // RPM073 (empty-conditional-branch) handles this case.
        let src = "Name: x\n%if 0\n%else\n%endif\n";
        assert!(run(src, EmptyElseDrop::new()).is_empty());
    }

    // ---- RPM082 invert-empty-if-arch ----

    #[test]
    fn rpm082_flags_empty_ifarch_branch() {
        let src = "Name: x\n%ifarch x86_64\n%else\nBuildArch: noarch\n%endif\n";
        let diags = run(src, InvertEmptyIfArch::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM082");
        assert!(diags[0].message.contains("ifnarch"));
    }

    #[test]
    fn rpm082_silent_for_plain_if() {
        // Only fires on arch/os kinds.
        let src = "Name: x\n%if 1\n%else\nBuildArch: noarch\n%endif\n";
        assert!(run(src, InvertEmptyIfArch::new()).is_empty());
    }

    #[test]
    fn rpm082_silent_when_if_branch_has_content() {
        let src = "Name: x\n%ifarch x86_64\nVersion: 1\n%else\nBuildArch: noarch\n%endif\n";
        assert!(run(src, InvertEmptyIfArch::new()).is_empty());
    }

    #[test]
    fn rpm082_silent_when_no_else() {
        let src = "Name: x\n%ifarch x86_64\n%endif\n";
        assert!(run(src, InvertEmptyIfArch::new()).is_empty());
    }

    // ---- RPM085 constant-tautology-in-expr ----

    #[test]
    fn rpm085_flags_or_one() {
        let src = "Name: x\n%if 0 || 1\nVersion: 1\n%endif\n";
        let diags = run(src, ConstantTautologyInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always-true"));
    }

    #[test]
    fn rpm085_flags_and_zero() {
        let src = "Name: x\n%if 1 && 0\nVersion: 1\n%endif\n";
        let diags = run(src, ConstantTautologyInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always-false"));
    }

    #[test]
    fn rpm085_flags_true_or() {
        let src = "Name: x\n%if true || 0\nVersion: 1\n%endif\n";
        let diags = run(src, ConstantTautologyInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm085_silent_for_normal_or() {
        let src = "Name: x\n%if 0 || 0\nVersion: 1\n%endif\n";
        // `0 || 0` is always false but RPM072 already catches constant
        // conditions; RPM085 should not also fire on it (no `|| 1` or
        // `&& 0` pattern).
        //
        // Wait — `&& 0` matches `0 || 0`? No, `||` not `&&`.
        // `0 || 0` -> normalised "0||0" -> looking for "||1" or "1||" -> none.
        // -> looking for "&&0" or "0&&" -> "0&&" not present (we have "0||0").
        // OK, silent.
        assert!(run(src, ConstantTautologyInExpr::new()).is_empty());
    }

    #[test]
    fn rpm085_silent_for_macro_expression() {
        let src = "Name: x\n%if 0%{?rhel} || 1\nVersion: 1\n%endif\n";
        // Bails on `%` — conservative.
        assert!(run(src, ConstantTautologyInExpr::new()).is_empty());
    }

    #[test]
    fn rpm085_silent_for_non_constant_operands() {
        // Both sides are non-constant identifiers; nothing to flag.
        // (The earlier raw-text detector worried about substring
        // confusion like `|| 10` vs `|| 1`; with the AST path that's
        // a non-issue — `10` is correctly identified as the truthy
        // integer it is.)
        let src = "Name: x\n%if X || Y\nVersion: 1\n%endif\n";
        assert!(run(src, ConstantTautologyInExpr::new()).is_empty());
    }

    // ---- RPM087 double-negation-in-expr ----

    #[test]
    fn rpm087_flags_double_bang() {
        let src = "Name: x\n%if !!X\nVersion: 1\n%endif\n";
        let diags = run(src, DoubleNegationInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM087");
    }

    #[test]
    fn rpm087_flags_double_bang_with_macro() {
        // `!!` is itself non-macro text; works even if X is a macro.
        let src = "Name: x\n%if !!0%{?rhel}\nVersion: 1\n%endif\n";
        let diags = run(src, DoubleNegationInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm087_silent_for_single_negation() {
        let src = "Name: x\n%if !X\nVersion: 1\n%endif\n";
        assert!(run(src, DoubleNegationInExpr::new()).is_empty());
    }

    #[test]
    fn rpm087_silent_for_not_equal() {
        // `!=` contains `!` but no `!!`.
        let src = "Name: x\n%if X != 1\nVersion: 1\n%endif\n";
        assert!(run(src, DoubleNegationInExpr::new()).is_empty());
    }

    // ---- RPM083 collapse-elif-into-else ----

    #[test]
    fn rpm083_flags_final_elif_true() {
        let src = "Name: x\n%if 0\nLicense: MIT\n%elif 1\nLicense: GPL\n%endif\n";
        let diags = run(src, CollapseElifIntoElse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM083");
    }

    #[test]
    fn rpm083_silent_when_already_has_else() {
        let src =
            "Name: x\n%if 0\nLicense: MIT\n%elif 1\nLicense: GPL\n%else\nLicense: BSD\n%endif\n";
        assert!(run(src, CollapseElifIntoElse::new()).is_empty());
    }

    #[test]
    fn rpm083_silent_when_elif_not_constant_true() {
        let src = "Name: x\n%if 0\nLicense: MIT\n%elif X\nLicense: GPL\n%endif\n";
        assert!(run(src, CollapseElifIntoElse::new()).is_empty());
    }

    // ---- RPM086 idempotent-in-expr ----

    #[test]
    fn rpm086_flags_x_and_x() {
        let src = "Name: x\n%if 5 && 5\nLicense: MIT\n%endif\n";
        let diags = run(src, IdempotentInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM086");
    }

    #[test]
    fn rpm086_silent_for_distinct_operands() {
        let src = "Name: x\n%if 1 && 2\nLicense: MIT\n%endif\n";
        assert!(run(src, IdempotentInExpr::new()).is_empty());
    }

    // ---- RPM088 self-comparison-in-expr ----

    #[test]
    fn rpm088_flags_self_eq() {
        let src = "Name: x\n%if 5 == 5\nLicense: MIT\n%endif\n";
        let diags = run(src, SelfComparisonInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always-true"));
    }

    #[test]
    fn rpm088_flags_self_lt() {
        let src = "Name: x\n%if 5 < 5\nLicense: MIT\n%endif\n";
        let diags = run(src, SelfComparisonInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always-false"));
    }

    #[test]
    fn rpm088_silent_for_distinct_operands() {
        let src = "Name: x\n%if 5 == 4\nLicense: MIT\n%endif\n";
        assert!(run(src, SelfComparisonInExpr::new()).is_empty());
    }

    // ---- RPM094 line-continuation-in-condition ----

    #[test]
    fn rpm094_flags_continuation() {
        // `%if A \<NL>  B` joins to a literal containing `\n`.
        // The parser falls back to Raw because the joined text isn't
        // valid expression grammar.
        let src = "Name: x\n%if A \\\n  B\nLicense: MIT\n%endif\n";
        let diags = run(src, LineContinuationInCondition::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM094");
    }

    #[test]
    fn rpm094_silent_for_normal_if() {
        let src = "Name: x\n%if 1\nLicense: MIT\n%endif\n";
        assert!(run(src, LineContinuationInCondition::new()).is_empty());
    }

    // ---- RPM100 collapse-else-if-into-elif ----

    #[test]
    fn rpm100_flags_else_holding_single_if() {
        let src = "Name: x\n%if A\nLicense: MIT\n%else\n%if B\nLicense: GPL\n%endif\n%endif\n";
        let diags = run(src, CollapseElseIfIntoElif::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM100");
    }

    #[test]
    fn rpm100_silent_when_else_has_more_content() {
        let src = "Name: x\n%if A\nLicense: MIT\n%else\nLicense: BSD\n%if B\nLicense: GPL\n%endif\n%endif\n";
        assert!(run(src, CollapseElseIfIntoElif::new()).is_empty());
    }

    #[test]
    fn rpm100_silent_when_no_else() {
        let src = "Name: x\n%if A\n%if B\nLicense: GPL\n%endif\n%endif\n";
        assert!(run(src, CollapseElseIfIntoElif::new()).is_empty());
    }

    // ---- RPM101 absorption-in-expr ----

    #[test]
    fn rpm101_flags_or_absorption() {
        // `5 || (5 && 6)` → can be reduced to `5`. Use integer
        // literals to avoid macro bail-out; absorption is a pure
        // boolean-algebra reduction.
        let src = "Name: x\n%if 5 || (5 && 6)\nLicense: MIT\n%endif\n";
        let diags = run(src, AbsorptionInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM101");
    }

    #[test]
    fn rpm101_flags_and_absorption() {
        let src = "Name: x\n%if 5 && (5 || 6)\nLicense: MIT\n%endif\n";
        let diags = run(src, AbsorptionInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm101_silent_for_independent_operands() {
        let src = "Name: x\n%if 5 || (6 && 7)\nLicense: MIT\n%endif\n";
        assert!(run(src, AbsorptionInExpr::new()).is_empty());
    }

    // ---- RPM104 string-set-redundancy ----

    #[test]
    fn rpm104_flags_repeated_string_in_or() {
        let src = "Name: x\n%if %{?_vendor} == \"a\" || %{?_vendor} == \"b\" || %{?_vendor} == \"a\"\nLicense: MIT\n%endif\n";
        let diags = run(src, StringSetRedundancy::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM104");
    }

    #[test]
    fn rpm104_silent_for_unique_strings() {
        let src =
            "Name: x\n%if %{?_vendor} == \"a\" || %{?_vendor} == \"b\"\nLicense: MIT\n%endif\n";
        assert!(run(src, StringSetRedundancy::new()).is_empty());
    }

    #[test]
    fn rpm104_silent_for_different_lhs() {
        // Same literal `"a"` but compared against different macros — not a dupe.
        let src = "Name: x\n%if %{?x} == \"a\" || %{?y} == \"a\"\nLicense: MIT\n%endif\n";
        assert!(run(src, StringSetRedundancy::new()).is_empty());
    }

    // ---- RPM105 inverted-if-else ----

    #[test]
    fn rpm105_flags_negated_if_with_else() {
        let src = "Name: x\n%if !X\nLicense: MIT\n%else\nLicense: GPL\n%endif\n";
        let diags = run(src, InvertedIfElse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM105");
    }

    #[test]
    fn rpm105_silent_for_non_negated_if() {
        let src = "Name: x\n%if X\nLicense: MIT\n%else\nLicense: GPL\n%endif\n";
        assert!(run(src, InvertedIfElse::new()).is_empty());
    }

    #[test]
    fn rpm105_silent_when_no_else() {
        let src = "Name: x\n%if !X\nLicense: MIT\n%endif\n";
        assert!(run(src, InvertedIfElse::new()).is_empty());
    }

    #[test]
    fn rpm105_silent_for_not_equal_op() {
        // `X != 1` starts with `X`, not `!`. Should not trigger.
        let src = "Name: x\n%if X != 1\nLicense: MIT\n%else\nLicense: GPL\n%endif\n";
        assert!(run(src, InvertedIfElse::new()).is_empty());
    }
}
