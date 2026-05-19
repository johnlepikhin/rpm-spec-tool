pub mod ast;
pub mod check;
pub mod completions;
pub mod config;
pub mod config_loader;
pub mod format;
pub mod lint;
pub mod lints;
pub mod matrix;
pub mod pretty;
pub mod printer_config;
pub mod profile;
pub mod repo;
pub mod target;

/// Upper bound on the per-level conditional indent. The printer
/// renders the indent literally as `n * level` spaces; without a
/// cap, a malicious or mistyped value (e.g. `--indent 4000000000`)
/// would push billions of spaces into the output. 64 spaces per
/// level is already absurd for human review.
///
/// Shared by `format` and `pretty` so both honour the same ceiling.
pub const MAX_INDENT_LEVEL: u32 = 64;
