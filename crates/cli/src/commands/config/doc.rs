//! `config doc` — render the JSON Schema as a human-readable
//! markdown reference page.
//!
//! Walks the same `schema_for!(Config)` tree the schema subcommand
//! emits and produces a flat field list grouped by section. Source of
//! truth is the doc comments on the `Config` struct family — what
//! `cargo doc` sees, what the LSP shows on hover, and what this page
//! prints are all the same text.

use std::io::Write;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use rpm_spec_analyzer::config::Config;
use serde_json::Value;

#[derive(Debug, Args)]
pub struct DocOpts {
    /// Restrict the output to a single top-level section (`lints`,
    /// `format`, `shellcheck`, `profiles`, `targets`). Without this
    /// flag the whole reference page is printed.
    #[arg(long, value_name = "NAME")]
    pub field: Option<String>,
}

pub fn run(opts: DocOpts) -> Result<ExitCode> {
    let schema = schemars::schema_for!(Config);
    let root = serde_json::to_value(&schema).context("serialize schema")?;
    let body = render(&root, opts.field.as_deref());
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(body.as_bytes())
        .context("write to stdout")?;
    Ok(ExitCode::SUCCESS)
}

/// Render the full document, or the subtree under `only_field` if set.
fn render(root: &Value, only_field: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("# `.rpmspec.toml` reference\n\n");
    if only_field.is_none() {
        out.push_str(
            "Generated from the JSON Schema of `rpm_spec_analyzer::config::Config`. \
             Every field below shows its TOML name, type, default, and \
             description sourced from the struct doc comments.\n\n",
        );
    }
    let defs = root.get("$defs").and_then(Value::as_object);
    let Some(props) = root.get("properties").and_then(Value::as_object) else {
        out.push_str("_(empty schema)_\n");
        return out;
    };
    let mut entries: Vec<(&String, &Value)> = props.iter().collect();
    entries.sort_by_key(|(k, _)| k.as_str());
    for (name, schema) in entries {
        if let Some(only) = only_field
            && only != name
        {
            continue;
        }
        render_field(&mut out, name, schema, defs, 2);
    }
    out
}

/// Render one field as a level-`depth` heading plus a property block.
/// `defs` is the `$defs` map at the schema root — we follow `$ref`s
/// into it so the user sees nested struct layouts inline.
fn render_field(
    out: &mut String,
    name: &str,
    schema: &Value,
    defs: Option<&serde_json::Map<String, Value>>,
    depth: usize,
) {
    let resolved = resolve_ref(schema, defs);
    let target = resolved.as_ref().unwrap_or(schema);
    let description = field_description(schema, target);
    let ty = field_type(schema, target);
    let default = field_default(schema, target);

    let heading = "#".repeat(depth.min(6));
    out.push_str(&format!("{heading} `{name}`\n\n"));
    out.push_str(&format!("- **Type:** `{ty}`\n"));
    if let Some(d) = default {
        out.push_str(&format!("- **Default:** `{d}`\n"));
    }
    if let Some(desc) = description {
        out.push_str(&format!("- **Description:** {desc}\n"));
    }
    // Walk nested object properties one level deep so users see the
    // schema of `[profiles.X]` etc. inline without chasing `$ref`s.
    if let Some(nested_props) = target.get("properties").and_then(Value::as_object)
        && depth < 4
    {
        out.push('\n');
        let mut nested: Vec<(&String, &Value)> = nested_props.iter().collect();
        nested.sort_by_key(|(k, _)| k.as_str());
        for (n, s) in nested {
            render_field(out, n, s, defs, depth + 1);
        }
    }
    out.push('\n');
}

fn resolve_ref(schema: &Value, defs: Option<&serde_json::Map<String, Value>>) -> Option<Value> {
    let raw = schema.get("$ref")?.as_str()?;
    let key = raw.strip_prefix("#/$defs/")?;
    defs?.get(key).cloned()
}

fn field_description(original: &Value, target: &Value) -> Option<String> {
    // Description on the property reference wins (per-use override);
    // otherwise fall back to the referenced type's own description.
    original
        .get("description")
        .or_else(|| target.get("description"))
        .and_then(Value::as_str)
        .map(|s| s.trim().replace('\n', " "))
}

fn field_default(original: &Value, target: &Value) -> Option<String> {
    let v = original.get("default").or_else(|| target.get("default"))?;
    Some(format_default(v))
}

fn format_default(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("\"{s}\""),
        Value::Array(a) if a.is_empty() => "[]".to_string(),
        Value::Object(o) if o.is_empty() => "{}".to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn field_type(original: &Value, target: &Value) -> String {
    // Prefer the *referenced* type name when available — telling users
    // "FormatConfig" is more useful than "object".
    if let Some(raw) = original.get("$ref").and_then(Value::as_str)
        && let Some(name) = raw.strip_prefix("#/$defs/")
    {
        return name.to_string();
    }
    if let Some(t) = target.get("type").and_then(Value::as_str) {
        // Decorate arrays + objects so the user sees the item type.
        if t == "array"
            && let Some(item_ty) = target
                .get("items")
                .and_then(|i| i.get("type").and_then(Value::as_str))
        {
            return format!("array<{item_ty}>");
        }
        if t == "object"
            && let Some(_) = target.get("additionalProperties")
        {
            let v_ty = additional_type(target.get("additionalProperties").unwrap());
            return format!("map<string, {v_ty}>");
        }
        return t.to_string();
    }
    "any".to_string()
}

fn additional_type(v: &Value) -> String {
    if let Some(raw) = v.get("$ref").and_then(Value::as_str)
        && let Some(name) = raw.strip_prefix("#/$defs/")
    {
        return name.to_string();
    }
    v.get("type")
        .and_then(Value::as_str)
        .unwrap_or("any")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_doc_includes_known_sections() {
        let schema = schemars::schema_for!(Config);
        let root = serde_json::to_value(&schema).unwrap();
        let md = render(&root, None);
        assert!(md.contains("`lints`"), "missing lints heading:\n{md}");
        assert!(md.contains("`format`"));
        assert!(md.contains("`shellcheck`"));
        assert!(md.contains("`profiles`"));
        assert!(md.contains("`targets`"));
    }

    #[test]
    fn field_filter_narrows_output() {
        let schema = schemars::schema_for!(Config);
        let root = serde_json::to_value(&schema).unwrap();
        let md = render(&root, Some("format"));
        // Only the requested top-level field appears.
        assert!(md.contains("`format`"));
        assert!(!md.contains("\n## `lints`\n"));
        // Nested field of FormatConfig is rendered inline.
        assert!(
            md.contains("preamble-align-column"),
            "expected nested field rendering:\n{md}"
        );
    }
}
