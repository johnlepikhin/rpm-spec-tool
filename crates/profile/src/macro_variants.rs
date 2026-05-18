//! `[macros.NAME]` schema — declared variant value sets used by the
//! coverage analyser to distinguish genuinely dead code from
//! build-conditional code.
//!
//! Lives in its own module rather than alongside the other
//! `.rpmspec.toml` types in [`crate::config_layer`] so coverage's
//! Phase B feature stays self-contained — adding a new field to
//! [`MacroVariants`] doesn't churn the rest of the config schema.

use serde::{Deserialize, Serialize};

/// Declared variant value set for a macro.
///
/// Coverage analysis (Phase B of `matrix coverage`) uses these to
/// distinguish two cases that look identical to a pure single-build
/// evaluator:
///
/// 1. **Genuinely dead code** — a branch that no member profile ever
///    activates under any plausible build configuration. Still tagged
///    `[DEAD]`.
/// 2. **Build-conditional code** — a branch inactive under the
///    current `-D`/profile combination but reachable when one of the
///    variant macros takes another declared value. Tagged
///    `[CONDITIONAL: macro=value]`.
///
/// Without variants the analyser can't tell those apart, and operators
/// either get a polluted dead-code list or have to manually rerun the
/// tool with each `-D` combination.
///
/// Example: `[macros.edition] values = ["ent", "std", "1c"]` lets
/// the tool report `%if "%{edition}" == "1c"` as
/// `[CONDITIONAL: edition=1c]` even when the current build uses
/// `-D edition ent`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct MacroVariants {
    /// Allowed values for the macro. An empty list is parsed but
    /// carries no reachability information — equivalent to omitting
    /// the entry entirely. The analyser does NOT inject these into
    /// the macro registry by default; they only participate in the
    /// variant-cartesian product when classifying inactive branches.
    pub values: Vec<String>,
    /// Optional one-line description shown in tool output and in the
    /// `config doc` rendering. Not load-bearing — purely informational.
    pub description: Option<String>,
}

impl MacroVariants {
    /// Construct a variant declaration with just the value set. The
    /// struct is `#[non_exhaustive]` so external crates can't
    /// struct-literal-build one; this constructor is the supported
    /// path. Future fields land here with their own setter
    /// (`with_description`, etc.) keeping the call site forward-
    /// compatible.
    pub fn new(values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            values: values.into_iter().map(Into::into).collect(),
            description: None,
        }
    }
}
