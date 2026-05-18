//! `target show <NAME>` — resolve and pretty-print one target set.

use std::io::Write;

use anyhow::Result;
use clap::Args;
use rpm_spec_analyzer::profile::{ResolvedTargetSet, TargetEntry};

use crate::app::MacroDefinesArg;
use crate::commands::profile::fmt::family_label;
use crate::commands::profile::style::Style;

#[derive(Debug, Args)]
pub struct ShowOpts {
    /// Name of the target set defined in `[targets.<name>]`.
    pub name: String,
    #[command(flatten)]
    pub defines: MacroDefinesArg,
}

/// Render the resolved target: top-level summary + per-profile rows
/// showing identity, macro counts and any defines layered for that
/// profile within the target.
pub(super) fn render(
    out: &mut impl Write,
    resolved: &ResolvedTargetSet,
    entry: &TargetEntry,
    style: &Style,
) -> Result<()> {
    writeln!(
        out,
        "{}",
        style.bold(&format!("# Target set: {}", resolved.id))
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "  profiles: {}",
        style.bold(&resolved.targets.len().to_string())
    )?;
    writeln!(out, "  target-wide defines: {}", entry.defines.len())?;
    writeln!(
        out,
        "  per-profile overrides: {}",
        entry.profile_overrides.len()
    )?;
    writeln!(out)?;

    writeln!(
        out,
        "  {} {} {} {} {} {}",
        style.bold(&format!("{:<28}", "PROFILE")),
        style.bold(&format!("{:<9}", "FAMILY")),
        style.bold(&format!("{:<12}", "VENDOR")),
        style.bold(&format!("{:<11}", "DIST-TAG")),
        style.bold(&format!("{:>6}", "MACROS")),
        style.bold("OVERRIDES"),
    )?;
    for rt in &resolved.targets {
        let overrides_label = entry
            .profile_overrides
            .get(&rt.profile_id)
            .map(|po| format!("defines:{}", po.defines.len()))
            .unwrap_or_else(|| "-".to_string());
        writeln!(
            out,
            "  {name:<28} {fam:<9} {vendor:<12} {dist:<11} {macros:>6}  {overrides_label}",
            name = rt.profile_id,
            fam = family_label(&rt.profile),
            vendor = rt.profile.identity.vendor.as_deref().unwrap_or("-"),
            dist = rt.profile.identity.dist_tag.as_deref().unwrap_or("-"),
            macros = rt.profile.macros.len(),
        )?;
    }

    if !entry.defines.is_empty() {
        writeln!(out)?;
        writeln!(out, "{}", style.bold("## target-wide defines"))?;
        for (k, v) in &entry.defines {
            writeln!(out, "  {k} = {v}")?;
        }
    }
    Ok(())
}
