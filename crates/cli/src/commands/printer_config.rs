//! Shared printer-config assembly for `format` and `pretty`.
//!
//! Both commands take the same overrides (`--preamble-align-column`,
//! `--indent`) on top of the user's `.rpmspec.toml`. The only place
//! they diverge is the **floor** on indent (pretty defaults to 2 for
//! display-mode readability; format respects the config value
//! verbatim) — that's handled by the caller after this helper runs.

use rpm_spec::printer::PrinterConfig;
use rpm_spec_analyzer::config::FormatConfig;

/// Apply the `--preamble-align-column` and `--indent` CLI overrides
/// on top of the analyzer's `FormatConfig`. Returns a fully-resolved
/// `PrinterConfig`. Callers may still post-process (e.g. apply a
/// display-mode indent floor).
pub fn apply_overrides(
    cfg: &FormatConfig,
    column_override: Option<u32>,
    indent_override: Option<u32>,
) -> PrinterConfig {
    let mut pcfg = cfg.to_printer_config();
    if let Some(col) = column_override {
        // `column_override == Some(0)` is a sentinel meaning
        // "no alignment" (single space between tag and value).
        pcfg = if col == 0 {
            pcfg.with_preamble_value_column(None)
        } else {
            pcfg.with_preamble_value_column(Some(col as usize))
        };
    }
    if let Some(n) = indent_override {
        // Unlike `column_override`, `Some(0)` here is *not* a sentinel
        // — it explicitly forces indent=0, overriding any config
        // setting. The clap value_parser caps `n` at
        // `MAX_INDENT_LEVEL`, so the `as usize` cast can't blow up.
        pcfg = pcfg.with_indent(n as usize);
    }
    pcfg
}
