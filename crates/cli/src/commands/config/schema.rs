//! `config schema` — emit the JSON Schema for `.rpmspec.toml`.
//!
//! Why JSON Schema:
//! * `taplo`, the de-facto TOML LSP, picks it up directly via a
//!   `.taplo.toml` mapping or a `#:schema` annotation at the top of
//!   the file.
//! * VS Code's "Even Better TOML", Helix, Zed all consume the same
//!   schema for inline completion + validation.
//! * Anyone who wants generated docs in their own format can post-
//!   process the JSON — `config doc` does exactly that.
//!
//! The schema is generated at runtime from `#[derive(JsonSchema)]`
//! annotations on the [`Config`] tree, so it can't drift from the
//! struct definitions.

use std::io::Write;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use rpm_spec_analyzer::config::Config;

#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum SchemaFormat {
    /// Pretty-printed JSON (default).
    Json,
    /// Compact JSON, no whitespace.
    JsonCompact,
}

#[derive(Debug, Args)]
pub struct SchemaOpts {
    /// Output format. Defaults to pretty-printed JSON.
    #[arg(long, default_value_t = SchemaFormat::Json, value_enum)]
    pub format: SchemaFormat,
}

pub fn run(opts: SchemaOpts) -> Result<ExitCode> {
    let schema = schemars::schema_for!(Config);
    let body = match opts.format {
        SchemaFormat::Json => serde_json::to_string_pretty(&schema).context("serialize schema")?,
        SchemaFormat::JsonCompact => serde_json::to_string(&schema).context("serialize schema")?,
    };
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{body}").context("write to stdout")?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_serializes_as_valid_json_with_known_fields() {
        let schema = schemars::schema_for!(Config);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        // Sanity checks: it's well-formed and mentions the public sections.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let props = parsed.get("properties").unwrap().as_object().unwrap();
        assert!(props.contains_key("lints"));
        assert!(props.contains_key("format"));
        assert!(props.contains_key("shellcheck"));
    }
}
