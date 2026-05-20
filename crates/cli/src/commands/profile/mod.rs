//! `profile` subcommand — inspect distribution profiles.
//!
//! Five modes:
//! * `profile show [NAME]` — pretty-print identity, layer trail, and
//!   counts for the active profile (or the named one if given);
//!   `--full` dumps the entire macro registry.
//! * `profile list` — tabular listing of every available profile
//!   (built-in + user-defined), with a marker on the active one.
//! * `profile macros [PROFILE]` — list a profile's macro registry,
//!   with optional `--filter` and `--source` narrowing.
//! * `profile macro <NAME> [PROFILES…]` — look up a single macro;
//!   behaviour scales with profile arg count (0 = table across all,
//!   1 = compact single, 2+ = comparison table).
//! * `profile common [PROFILES…]` — intersection of macros across
//!   profiles; existence by default, value-equality with `--mode value`.
//!
//! ## Exit codes
//!
//! * `0` — success (including legitimate empty result, e.g.
//!   `profile common` with no shared macros).
//! * `2` — soft user error: macro undefined (`profile macro NAME ONE`),
//!   `profile common` invoked with < 2 profiles, or a built-in failed
//!   to resolve during `profile list`.
//! * `1` — anyhow-bubbled error (typically `failed to resolve profile`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{Profile, ResolveOptions, builtin};

mod common;
pub(crate) mod fmt;
mod list;
mod macro_lookup;
mod macros;
mod show;
pub(crate) mod style;

// Re-export Opts so `Action` variants resolve them without paths and
// so external consumers (`commands/mod.rs`, integration tests) keep a
// stable surface.
pub use common::CommonOpts;
pub use list::ListOpts;
pub use macro_lookup::MacroOpts;
pub use macros::MacrosOpts;
pub use show::ShowOpts;

/// Fallback active-profile name when neither CLI nor config specifies one.
/// Matches `rpm_spec_profile::builtin::DEFAULT_BUILTIN`.
pub(super) const DEFAULT_PROFILE: &str = "generic";

#[derive(Debug, Args)]
pub struct Cmd {
    /// Explicit path to a `rpmspec.toml` config file. Without this
    /// flag the tool checks `$RPM_SPEC_TOOL_CONFIG` then
    /// `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`, falling back to
    /// built-in defaults.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Print the resolved profile to stdout.
    Show(ShowOpts),
    /// List every available profile (built-in + from config).
    List(ListOpts),
    /// List the macro registry of a profile, with optional filtering.
    Macros(MacrosOpts),
    /// Look up a macro's value. Behaviour scales with the number of
    /// profile arguments: with none, every available profile is shown
    /// as a comparison table; with one, the value is printed compactly
    /// (exit 2 if undefined); with two or more, only the named profiles
    /// are compared.
    Macro(MacroOpts),
    /// Print the intersection of macros across two or more profiles.
    /// Default mode: by existence (a macro is "common" if every profile
    /// defines it). With `--mode value`, also require identical
    /// values (`opts` + body; provenance is ignored).
    Common(CommonOpts),
}

impl Cmd {
    pub fn run(self, color: crate::app::ColorChoice) -> Result<ExitCode> {
        let (config, base_dir) =
            crate::commands::config_loader::load_config(self.config.as_deref())?;
        let style = style::Style::new(color);
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        match self.action {
            Action::Show(opts) => {
                let cli_override = opts.name.as_deref();
                let profile = config
                    .resolve_profile(
                        &base_dir,
                        ResolveOptions::with_override(cli_override).with_defines(&opts.defines.raw),
                    )
                    .with_context(|| "failed to resolve profile")?;
                show::render(&mut out, &profile, opts.full, &style)?;
            }
            Action::List(opts) => {
                return list::render_list(&mut out, &config, &base_dir, opts, &style);
            }
            Action::Macros(opts) => {
                let profile_name = opts.profile.as_deref();
                let profile = config
                    .resolve_profile(
                        &base_dir,
                        ResolveOptions::with_override(profile_name).with_defines(&opts.defines.raw),
                    )
                    .with_context(|| "failed to resolve profile")?;
                let effective_name = active_profile_name(profile_name, &config);
                macros::render_macros(&mut out, effective_name, &profile, &opts, &style)?;
            }
            Action::Macro(opts) => {
                return macro_lookup::dispatch_macro(&mut out, &config, &base_dir, opts, &style);
            }
            Action::Common(opts) => {
                return common::dispatch_common(&mut out, &config, &base_dir, opts, &style);
            }
        }
        Ok(ExitCode::SUCCESS)
    }
}

/// Active-profile name resolution: CLI override → config `profile = …`
/// → built-in default. Used by `Macros`/`Macro` dispatch and by
/// `render_list` to mark the active row.
pub(super) fn active_profile_name<'a>(
    cli_override: Option<&'a str>,
    config: &'a Config,
) -> &'a str {
    cli_override
        .or(config.profile.as_deref())
        .unwrap_or(DEFAULT_PROFILE)
}

/// All available profile names: every built-in (in registry order),
/// followed by user-defined profiles from the config that don't shadow
/// a built-in. User-defined names are appended in BTreeMap iteration
/// order (alphabetical). Used as the default for `profile macro` with
/// no explicit profile argument.
pub(super) fn all_profile_names(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = builtin::names().iter().map(|s| (*s).to_string()).collect();
    for key in config.profiles.keys() {
        if !names.iter().any(|n| n == key) {
            names.push(key.clone());
        }
    }
    names
}

/// Resolve every name into a `(name, Profile)` pair, surfacing the
/// first failure with context. Shared by `dispatch_macro` and
/// `dispatch_common` so the two paths can't drift on error wording.
///
/// `defines` is applied to every resolved profile — `profile macro X P1
/// P2 -D 'foo bar'` injects `foo=bar` across all of P1, P2 so the
/// comparison table reflects the user's `--define` against each
/// distribution's baseline.
pub(super) fn resolve_many(
    config: &Config,
    base_dir: &Path,
    names: &[String],
    defines: &[String],
) -> Result<Vec<(String, Profile)>> {
    names
        .iter()
        .map(|name| {
            let p = config
                .resolve_profile(
                    base_dir,
                    ResolveOptions::with_override(Some(name)).with_defines(defines),
                )
                .with_context(|| format!("failed to resolve profile `{name}`"))?;
            Ok((name.clone(), p))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_analyzer::profile::ProfileEntry;

    #[test]
    fn all_profile_names_builtin_first_then_user_alpha() {
        let mut config = Config::default();
        config
            .profiles
            .insert("zzz-user".to_string(), ProfileEntry::default());
        config
            .profiles
            .insert("aaa-user".to_string(), ProfileEntry::default());
        // Shadow an existing builtin — must not appear twice.
        config
            .profiles
            .insert("generic".to_string(), ProfileEntry::default());

        let names = all_profile_names(&config);
        // First entry is always `generic` (DEFAULT_BUILTIN in registry).
        assert_eq!(names[0], DEFAULT_PROFILE);
        // No duplicate for the shadowed name.
        assert_eq!(names.iter().filter(|n| *n == DEFAULT_PROFILE).count(), 1);
        // User-only profiles come after builtins, alphabetically.
        let aaa_pos = names.iter().position(|n| n == "aaa-user").unwrap();
        let zzz_pos = names.iter().position(|n| n == "zzz-user").unwrap();
        let last_builtin_pos = names
            .iter()
            .rposition(|n| builtin::names().contains(&n.as_str()))
            .unwrap();
        assert!(
            aaa_pos > last_builtin_pos,
            "user profiles must follow builtins"
        );
        assert!(aaa_pos < zzz_pos, "user profiles must be alphabetical");
    }
}
