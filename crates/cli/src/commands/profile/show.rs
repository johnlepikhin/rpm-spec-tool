//! `profile show` — pretty-print identity, layer trail, and counts for
//! a single resolved profile. `--full` dumps the entire macro registry.

use std::io::Write;

use anyhow::Result;
use clap::Args;
use rpm_spec_analyzer::profile::{LayerInfo, Profile};

use super::fmt::{format_macro_value_inline, format_opts, format_provenance};
use super::style::Style;

#[derive(Debug, Args)]
pub struct ShowOpts {
    /// Profile name to resolve. Defaults to the `profile = …` entry in
    /// the loaded config, or `generic` if neither the config nor this
    /// argument specify one.
    pub name: Option<String>,
    /// Dump every macro entry in the resolved registry. Without this,
    /// only counts and a small selection are shown.
    #[arg(long)]
    pub full: bool,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
}

pub(super) fn render(
    out: &mut impl Write,
    profile: &Profile,
    full: bool,
    style: &Style,
) -> Result<()> {
    writeln!(out, "{}", style.bold("# Profile"))?;
    writeln!(out, "name:     {}", style.bold_cyan(&profile.identity.name))?;
    let family = profile
        .identity
        .family
        .map(|f| format!("{f:?}"))
        .unwrap_or_else(|| style.dim("(undetected)"));
    writeln!(out, "family:   {family}")?;
    writeln!(
        out,
        "vendor:   {}",
        profile
            .identity
            .vendor
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| style.dim("(unset)"))
    )?;
    writeln!(
        out,
        "dist-tag: {}",
        profile
            .identity
            .dist_tag
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| style.dim("(unset)"))
    )?;
    writeln!(out)?;

    writeln!(
        out,
        "{}",
        style.bold(&format!("# Layers ({})", profile.layers.len()))
    )?;
    for layer in &profile.layers {
        match layer {
            LayerInfo::Builtin { name } => writeln!(out, "  - builtin: {name}")?,
            LayerInfo::BuiltinShowrc { name, macros } => writeln!(
                out,
                "  - builtin-showrc: {name} {}",
                style.dim(&format!("({macros} macros)"))
            )?,
            LayerInfo::Showrc { path, macros } => writeln!(
                out,
                "  - showrc:  {} {}",
                path.display(),
                style.dim(&format!("({macros} macros)"))
            )?,
            LayerInfo::Override { fields } => {
                writeln!(out, "  - override: {} field(s)", fields.len())?;
                for f in fields {
                    writeln!(out, "      · {f}")?;
                }
            }
            LayerInfo::CliDefine { names } => {
                writeln!(out, "  - cli defines: {}", names.join(", "))?;
            }
            // LayerInfo is #[non_exhaustive]; render unknown variants
            // generically so an upgrade that adds a new layer doesn't
            // crash the CLI.
            _ => writeln!(out, "  - {}", style.dim("<unknown layer>"))?,
        }
    }
    writeln!(out)?;

    writeln!(out, "{}", style.bold("# Counts"))?;
    writeln!(out, "macros:   {}", profile.macros.len())?;
    writeln!(out, "rpmlib:   {}", profile.rpmlib.features.len())?;
    writeln!(
        out,
        "licenses: {} {}",
        profile.licenses.allowed.len(),
        style.dim(&format!("(mode = {:?})", profile.licenses.mode))
    )?;
    writeln!(
        out,
        "groups:   {} {}",
        profile.groups.allowed.len(),
        style.dim(&format!("(mode = {:?})", profile.groups.mode))
    )?;
    if let Some(arch) = &profile.arch.build_arch {
        writeln!(out, "arch:     {arch}")?;
    }
    if let Some(os) = &profile.arch.build_os {
        writeln!(out, "os:       {os}")?;
    }

    if full {
        writeln!(out)?;
        writeln!(out, "{}", style.bold("# Macros"))?;
        for (name, entry) in &profile.macros.entries {
            writeln!(
                out,
                "{name}{} = {}  {}",
                format_opts(entry.opts.as_deref()),
                format_macro_value_inline(&entry.value),
                style.dim(&format!("[{}]", format_provenance(&entry.provenance))),
            )?;
        }
    }
    Ok(())
}

// No unit tests in this module — formatting helpers are covered in
// `fmt::tests`; layer-trail rendering is exercised by the CLI
// integration tests (`profile_show_*` in `tests/cli.rs`).
