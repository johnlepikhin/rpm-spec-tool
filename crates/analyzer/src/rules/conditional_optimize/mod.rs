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
//! - RPM083 `collapse-elif-into-else` — final `%elif` with a
//!   constant-true expression is equivalent to `%else`.
//! - RPM085 `constant-tautology-in-expr` — `%if X || 1`, `%if X && 0`,
//!   and friends. The expression has a constant operand that fixes
//!   the result.
//! - RPM086 `idempotent-in-expr` — `X && X` / `X || X`.
//! - RPM087 `double-negation-in-expr` — `%if !!X` → `%if X`.
//! - RPM088 `self-comparison-in-expr` — `X == X`, `X < X`, etc.
//! - RPM094 `line-continuation-in-condition` — `\` inside `%if` text.
//! - RPM100 `collapse-else-if-into-elif` — `%else %if B ... %endif`
//!   → `%elif B`.
//! - RPM101 `absorption-in-expr` — `A || (A && B)` → `A`.
//! - RPM104 `string-set-redundancy` — duplicate `X == "lit"` in `||`.
//! - RPM105 `inverted-if-else` — `%if !X foo %else bar %endif`.

mod absorption;
mod elif_collapse;
mod else_if_elif;
mod empty_else;
mod expr_constants;
mod idempotent_self;
mod invert_arch;
mod inverted_if_else;
mod line_continuation;
mod nested_and;
mod string_set;

pub use absorption::AbsorptionInExpr;
pub use elif_collapse::CollapseElifIntoElse;
pub use else_if_elif::CollapseElseIfIntoElif;
pub use empty_else::EmptyElseDrop;
pub use expr_constants::{ConstantTautologyInExpr, DoubleNegationInExpr};
pub use idempotent_self::{IdempotentInExpr, SelfComparisonInExpr};
pub use invert_arch::InvertEmptyIfArch;
pub use inverted_if_else::InvertedIfElse;
pub use line_continuation::LineContinuationInCondition;
pub use nested_and::NestedAndCollapse;
pub use string_set::StringSetRedundancy;
