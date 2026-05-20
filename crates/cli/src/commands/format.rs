//! `format` subcommand — pretty-print spec files using `rpm_spec::printer`.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, value_parser};
use codespan_reporting::term::termcolor::{Color, ColorSpec, StandardStream, WriteColor};
use rpm_spec::printer::print_with;
use rpm_spec_analyzer::parse;

use crate::app::ColorChoice;
use crate::commands::{MAX_INDENT_LEVEL, printer_config};
use crate::config::{self as cli_config, ConfigCacheCliExt as _};
use crate::io;
use crate::output::resolve_color;

/// Source of an active non-zero indent. Used to pick the right phrase
/// in the cosmetic-only warning.
enum IndentSource {
    Cli,
    Config,
}

/// Tail of the warning, shared between CLI- and config-sourced cases.
const INDENT_COSMETIC_WARNING: &str = "is cosmetic only; rpm does not accept indented %if directives. \
     Do not commit the formatted output.";

#[derive(Debug, Args)]
pub struct Cmd {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    /// Exit non-zero if any file would change. Implies no writes.
    #[arg(long)]
    pub check: bool,

    /// Overwrite files with the formatted output.
    #[arg(long)]
    pub in_place: bool,

    /// Print a unified diff between input and formatted output.
    #[arg(long)]
    pub diff: bool,

    /// Override the preamble value alignment column (defaults to config).
    #[arg(long)]
    pub preamble_align_column: Option<u32>,

    /// Indent nested %if/%else/%endif blocks by N spaces per level (default 0).
    ///
    /// Cosmetic only: rpm rejects indented %if directives. Use for
    /// review, not for commits. Emits a stderr warning when N > 0.
    #[arg(long, value_parser = value_parser!(u32).range(0..=MAX_INDENT_LEVEL as i64))]
    pub indent: Option<u32>,
}

impl Cmd {
    pub fn run(self, color: ColorChoice) -> Result<ExitCode> {
        let sources = io::read_sources(&self.input.paths)?;
        let mut config_cache = cli_config::make_config_cache(self.input.config.clone());

        let mut would_change = false;
        let mut any_io_error = false;
        // Print the cosmetic-indent warning at most once per command
        // invocation, regardless of how many source files are
        // processed and whether the indent comes from `--indent` or
        // from `[format].conditional_indent` in a `.rpmspec.toml`.
        let mut indent_warning_emitted = false;

        for source in sources {
            let Some(analyzer_cfg) = config_cache.load_or_report(&source.path, &mut any_io_error)
            else {
                continue;
            };
            let pcfg = printer_config::apply_overrides(
                &analyzer_cfg.format,
                self.preamble_align_column,
                self.indent,
            );
            if !indent_warning_emitted && pcfg.indent > 0 {
                let source_label = if self.indent.is_some_and(|n| n > 0) {
                    IndentSource::Cli
                } else {
                    IndentSource::Config
                };
                emit_indent_warning(source_label);
                indent_warning_emitted = true;
            }

            let outcome = parse(&source.contents);
            let formatted = print_with(&outcome.spec, &pcfg);

            let changed = formatted != source.contents;
            if changed {
                would_change = true;
            }

            if self.check {
                if changed {
                    eprintln!("would reformat: {}", source.display_name());
                }
            } else if self.in_place && !source.is_stdin {
                if changed && let Err(e) = io::write_atomic(&source.path, &formatted) {
                    eprintln!("error writing {}: {e:#}", source.display_name());
                    any_io_error = true;
                }
            } else if self.diff {
                emit_diff(&source.display_name(), &source.contents, &formatted, color)?;
            } else {
                print!("{formatted}");
            }
        }

        Ok(if any_io_error {
            ExitCode::from(2)
        } else if self.check && would_change {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }
}

fn emit_indent_warning(source: IndentSource) {
    let label = match source {
        IndentSource::Cli => "--indent > 0",
        IndentSource::Config => "[format].conditional_indent > 0",
    };
    eprintln!("warning: {label} {INDENT_COSMETIC_WARNING}");
}

fn emit_diff(name: &str, before: &str, after: &str, color: ColorChoice) -> std::io::Result<()> {
    use similar::{ChangeTag, TextDiff};

    let stream = StandardStream::stdout(resolve_color(color, || std::io::stdout().is_terminal()));
    let mut w = stream.lock();

    let mut hdr = ColorSpec::new();
    hdr.set_bold(true);
    let mut minus = ColorSpec::new();
    minus.set_fg(Some(Color::Red));
    let mut plus = ColorSpec::new();
    plus.set_fg(Some(Color::Green));

    w.set_color(&hdr)?;
    writeln!(w, "--- {name} (original)")?;
    writeln!(w, "+++ {name} (formatted)")?;
    w.reset()?;

    let diff = TextDiff::from_lines(before, after);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => write!(w, " {change}")?,
            ChangeTag::Delete => {
                w.set_color(&minus)?;
                write!(w, "-{change}")?;
                w.reset()?;
            }
            ChangeTag::Insert => {
                w.set_color(&plus)?;
                write!(w, "+{change}")?;
                w.reset()?;
            }
        }
    }
    Ok(())
}
