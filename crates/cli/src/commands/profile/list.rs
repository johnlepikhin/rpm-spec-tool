//! `profile list` — tabular catalogue of every available profile.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{ProfileEntry, ProfileSection, builtin};

use super::fmt::family_label;
use super::style::Style;
use super::{DEFAULT_PROFILE, active_profile_name};

#[derive(Debug, Args)]
pub struct ListOpts {
    /// Show only built-in profiles. Without this flag, both built-ins
    /// and user-defined profiles (from `.rpmspec.toml`) are listed.
    #[arg(long, conflicts_with = "user_only")]
    pub builtin_only: bool,
    /// Show only user-defined profiles from the loaded config.
    #[arg(long, conflicts_with = "builtin_only")]
    pub user_only: bool,
}

/// Render the `profile list` output: two sections (built-ins, user
/// entries), each as a fixed-width table. The active profile (from
/// `config.profile` or the `generic` fallback) is prefixed with `*`.
///
/// Returns `ExitCode::from(2)` if any built-in failed to resolve so the
/// failure isn't silenced under a green exit.
pub(super) fn render_list(
    out: &mut impl Write,
    config: &Config,
    base_dir: &Path,
    opts: ListOpts,
    style: &Style,
) -> Result<ExitCode> {
    let active = active_profile_name(None, config);
    let mut had_error = false;

    if !opts.user_only {
        let builtins = builtin::names();
        writeln!(
            out,
            "{}",
            style.bold(&format!("# Built-in profiles ({})", builtins.len()))
        )?;
        writeln!(out)?;
        // Pad-then-style so :<W$ width tracks visible characters only.
        writeln!(
            out,
            "  {} {} {} {} {}  {}",
            style.bold(&format!("{:<28}", "NAME")),
            style.bold(&format!("{:<9}", "FAMILY")),
            style.bold(&format!("{:<12}", "VENDOR")),
            style.bold(&format!("{:<11}", "DIST-TAG")),
            style.bold(&format!("{:>6}", "MACROS")),
            style.bold("ARCH"),
        )?;
        // Resolve each builtin against an *empty* config section so the
        // pure built-in identity is shown regardless of any user entry
        // that may shadow the name.
        let empty = ProfileSection::default();
        for &name in builtins {
            let prefix = if name == active {
                style.bold_green("*")
            } else {
                " ".to_string()
            };
            match rpm_spec_analyzer::profile::resolve_profile(
                &empty,
                base_dir,
                rpm_spec_analyzer::profile::ResolveOptions::with_override(Some(name)),
            ) {
                Ok(p) => {
                    writeln!(
                        out,
                        "{prefix} {name:<28} {fam:<9} {vendor:<12} {dist:<11} {macros:>6}  {arch}",
                        fam = family_label(&p),
                        vendor = p.identity.vendor.as_deref().unwrap_or("-"),
                        dist = p.identity.dist_tag.as_deref().unwrap_or("-"),
                        macros = p.macros.len(),
                        arch = p.arch.build_arch.as_deref().unwrap_or("-"),
                    )?;
                }
                Err(e) => {
                    writeln!(
                        out,
                        "{prefix} {name:<28} {}",
                        style.dim_red(&format!("(failed to resolve: {e})"))
                    )?;
                    eprintln!("error: failed to resolve built-in `{name}`: {e}");
                    had_error = true;
                }
            }
        }
        writeln!(out)?;
    }

    if !opts.builtin_only {
        let entries: Vec<(&String, &ProfileEntry)> = config.profiles.iter().collect();
        writeln!(
            out,
            "{}",
            style.bold(&format!("# User-defined profiles ({})", entries.len()))
        )?;
        if entries.is_empty() {
            writeln!(
                out,
                "  {}",
                style.dim("(none — define profiles in [profiles.<name>] in .rpmspec.toml)")
            )?;
            return Ok(if had_error {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            });
        }
        writeln!(out)?;
        writeln!(
            out,
            "  {} {} {}",
            style.bold(&format!("{:<24}", "NAME")),
            style.bold(&format!("{:<22}", "EXTENDS")),
            style.bold("DETAILS"),
        )?;
        for (name, entry) in entries {
            let prefix = if name == active {
                style.bold_green("*")
            } else {
                " ".to_string()
            };
            let extends = entry.extends.as_deref().unwrap_or(DEFAULT_PROFILE);
            let details = user_entry_details(entry);
            writeln!(out, "{prefix} {name:<24} {extends:<22} {details}")?;
        }
    }

    Ok(if had_error {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

/// One-line summary of a user-defined profile entry — surfaces the
/// non-default fields without resolving the showrc dump from disk.
fn user_entry_details(entry: &ProfileEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = &entry.showrc_file {
        parts.push(format!("showrc-file={}", p.display()));
    }
    let mut overrides: Vec<&str> = Vec::new();
    if entry.identity.family.is_some() {
        overrides.push("family");
    }
    if entry.identity.vendor.is_some() {
        overrides.push("vendor");
    }
    if entry.identity.dist_tag.is_some() {
        overrides.push("dist-tag");
    }
    if !entry.macros.is_empty() {
        overrides.push("macros");
    }
    if entry.licenses.is_some() {
        overrides.push("licenses");
    }
    if entry.groups.is_some() {
        overrides.push("groups");
    }
    if !overrides.is_empty() {
        parts.push(format!("overrides: {}", overrides.join(", ")));
    }
    if parts.is_empty() {
        "(no overrides)".to_string()
    } else {
        parts.join("; ")
    }
}
