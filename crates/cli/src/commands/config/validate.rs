//! `config validate` — parse a `.rpmspec.toml` and report errors.
//!
//! Mirrors the silent path that runs implicitly during `lint`/`check`,
//! but surfaces just the load result so CI pipelines and pre-commit
//! hooks can fail fast on a typo without running the full analyzer.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::config_cache::default_config_path;

use crate::app::ColorChoice;
use crate::output::resolve_color;
use codespan_reporting::diagnostic::{Diagnostic, Label};
use codespan_reporting::files::SimpleFile;
use codespan_reporting::term;
use codespan_reporting::term::termcolor::StandardStream;
use std::io::IsTerminal;

/// Environment-variable override mirroring [`crate::config`]. Kept in
/// sync so all entry points (lint/check/format and `config validate`)
/// agree on which file gets loaded.
const ENV_CONFIG_OVERRIDE: &str = "RPM_SPEC_TOOL_CONFIG";

#[derive(Debug, Args)]
pub struct ValidateOpts {
    /// `.rpmspec.toml` to validate. When omitted, walks upward from
    /// the current directory looking for one — mirrors the discovery
    /// every other subcommand performs.
    pub path: Option<PathBuf>,
}

pub fn run(
    opts: ValidateOpts,
    color: ColorChoice,
    config_override: Option<PathBuf>,
) -> Result<ExitCode> {
    // Resolution cascade — matches the loader in `crate::config`:
    //   1. positional `<path>` to `config validate`
    //   2. global `--config <path>` flag
    //   3. `$RPM_SPEC_TOOL_CONFIG` env var
    //   4. XDG default (only if the file actually exists)
    //   5. upward walk from cwd for `.rpmspec.toml`
    let candidate = opts
        .path
        .clone()
        .or(config_override)
        .or_else(|| std::env::var(ENV_CONFIG_OVERRIDE).ok().map(PathBuf::from))
        .or_else(|| default_config_path().filter(|p| p.exists()));

    let path = match candidate {
        Some(p) => p,
        None => match discover_upward()? {
            Some(p) => p,
            None => {
                eprintln!("error: no rpmspec.toml found");
                eprintln!(
                    "hint:  pass --config PATH, set RPM_SPEC_TOOL_CONFIG, \
                     or place one at ~/.config/rpm-spec-tool/rpmspec.toml"
                );
                return Ok(ExitCode::from(2));
            }
        },
    };

    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;

    match Config::from_toml_str(&text) {
        Ok(_) => {
            println!("ok: {}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            render_toml_error(&path, &text, &e, color)?;
            Ok(ExitCode::from(1))
        }
    }
}

/// Walk upward from the cwd looking for `.rpmspec.toml`. Returns
/// `None` once we hit the filesystem root without finding one.
fn discover_upward() -> Result<Option<PathBuf>> {
    let mut dir = std::env::current_dir().context("failed to read current directory")?;
    loop {
        let candidate = dir.join(".rpmspec.toml");
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

/// Pretty-print a TOML deserialization failure with codespan-reporting.
/// The error's reported byte span is honoured when present; otherwise
/// the whole file is highlighted with the bare message.
fn render_toml_error(
    path: &Path,
    text: &str,
    err: &toml::de::Error,
    color: ColorChoice,
) -> Result<()> {
    let file = SimpleFile::new(path.display().to_string(), text);
    let mut diag = Diagnostic::error().with_message(err.message().to_string());
    if let Some(span) = err.span() {
        // `toml::de::Error::span` returns a byte range over the source.
        diag = diag.with_labels(vec![Label::primary((), span)]);
    }
    let resolved = resolve_color(color, || std::io::stderr().is_terminal());
    let writer = StandardStream::stderr(resolved);
    let cfg = term::Config::default();
    term::emit(&mut writer.lock(), &cfg, &file, &diag).context("failed to render diagnostic")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn rejects_unknown_field() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        // `deny_unknown_fields` on Config means an unknown key is a
        // hard error — exactly the failure we want validate to surface.
        writeln!(tmp, "garbage_field = 1").unwrap();
        let path = tmp.path().to_path_buf();

        let opts = ValidateOpts { path: Some(path) };
        let code = run(opts, ColorChoice::Never, None).unwrap();
        assert_eq!(code, ExitCode::from(1));
    }

    #[test]
    fn accepts_default_init_output() {
        // Anything `config init` writes must validate. Tests that the
        // two halves of the command stay in sync.
        let body = super::super::init::render(&super::super::init::InitOpts {
            output: None,
            profile: None,
            all_lints: false,
            stdout: false,
            force: false,
            yes: false,
            dry_run: false,
        })
        .unwrap();

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(body.as_bytes()).unwrap();
        let path = tmp.path().to_path_buf();

        let opts = ValidateOpts { path: Some(path) };
        let code = run(opts, ColorChoice::Never, None).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }
}
