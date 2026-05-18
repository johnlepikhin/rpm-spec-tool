//! `target list` — catalogue of release target sets.

use std::io::Write;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use rpm_spec_analyzer::config::Config;

use crate::commands::profile::style::Style;

#[derive(Debug, Args)]
pub struct ListOpts {}

/// Render a tabular listing of every `[targets.<name>]` section in
/// the loaded config. Empty config produces a hint line — release
/// matrix is opt-in, so silence would be confusing.
pub(super) fn render_list(
    out: &mut impl Write,
    config: &Config,
    _opts: ListOpts,
    style: &Style,
) -> Result<ExitCode> {
    writeln!(
        out,
        "{}",
        style.bold(&format!("# Release target sets ({})", config.targets.len()))
    )?;
    if config.targets.is_empty() {
        writeln!(
            out,
            "  {}",
            style.dim("(none — define target sets in [targets.<name>] in .rpmspec.toml)")
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    writeln!(out)?;
    writeln!(
        out,
        "  {} {} {} {}",
        style.bold(&format!("{:<28}", "NAME")),
        style.bold(&format!("{:>9}", "PROFILES")),
        style.bold(&format!("{:>9}", "DEFINES")),
        style.bold("DETAILS"),
    )?;
    for (name, entry) in &config.targets {
        let overrides_count = entry.profile_overrides.len();
        let details = if overrides_count > 0 {
            format!("per-profile overrides: {overrides_count}")
        } else {
            "(no per-profile overrides)".to_string()
        };
        writeln!(
            out,
            "  {name:<28} {profiles:>9} {defines:>9}  {details}",
            profiles = entry.profiles.len(),
            defines = entry.defines.len(),
        )?;
    }
    Ok(ExitCode::SUCCESS)
}
