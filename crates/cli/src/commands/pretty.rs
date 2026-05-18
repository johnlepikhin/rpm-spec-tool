//! `pretty` subcommand — print spec files with ANSI syntax highlighting.
//!
//! Like `format`, this delegates to `rpm_spec::printer`, but routes
//! output through an [`AnsiWriter`] that classifies each chunk by
//! [`TokenKind`] and applies a colour theme. Auto-mode emits colour
//! on a TTY and strips it on a pipe — matching `clippy` / `bat`.

use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, value_parser};
use codespan_reporting::term::termcolor::StandardStream;
use rpm_spec::printer::print_to;
use rpm_spec_analyzer::parse;

use crate::app::ColorChoice;
use crate::commands::{MAX_INDENT_LEVEL, printer_config};
use crate::config::{self as cli_config, ConfigCacheCliExt as _};
use crate::io;
use crate::output::pretty::{AnsiWriter, Theme};
use crate::output::resolve_color;

/// Default per-level indent for `pretty`. Display mode prefers
/// readability over rpm-source-compatibility, so a non-zero floor is
/// applied when the user (or their config) hasn't asked for a
/// specific indent.
const DEFAULT_PRETTY_INDENT: usize = 2;

#[derive(Debug, Args)]
pub struct Cmd {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    /// Override the preamble value alignment column (defaults to config).
    #[arg(long)]
    pub preamble_align_column: Option<u32>,

    /// Indent nested %if/%else/%endif blocks by N spaces per level.
    ///
    /// Defaults to `2` — `pretty` is a display mode, not a file
    /// round-trip target, so the extra readability is worth the
    /// rpm-incompatibility (rpm rejects indented `%if` directives).
    #[arg(long, value_parser = value_parser!(u32).range(0..=MAX_INDENT_LEVEL as i64))]
    pub indent: Option<u32>,
}

impl Cmd {
    pub fn run(self, color: ColorChoice) -> Result<ExitCode> {
        let sources = io::read_sources(&self.input.paths)?;
        let mut config_cache = cli_config::ConfigCache::new(self.input.config.clone());

        // Pretty writes to stdout — TTY detection must look at stdout,
        // not stderr (unlike `human.rs`).
        let stream =
            StandardStream::stdout(resolve_color(color, || std::io::stdout().is_terminal()));
        let mut writer = stream.lock();
        // Single sink for all sources so a latched I/O error survives
        // across file boundaries and a broken pipe short-circuits the
        // remaining inputs (no point tokenising after the reader left).
        let mut sink = AnsiWriter::new(&mut writer, Theme::dark());

        let mut any_io_error = false;
        for source in sources {
            let Some(analyzer_cfg) = config_cache.load_or_report(&source.path, &mut any_io_error)
            else {
                continue;
            };
            let mut pcfg = printer_config::apply_overrides(
                &analyzer_cfg.format,
                self.preamble_align_column,
                self.indent,
            );
            // Display-mode floor: when neither CLI nor config has
            // requested a non-zero indent, fall back to the pretty
            // default. An explicit `--indent 0` is honoured (the
            // override already wrote 0 into pcfg).
            if self.indent.is_none() && pcfg.indent == 0 {
                pcfg = pcfg.with_indent(DEFAULT_PRETTY_INDENT);
            }
            let outcome = parse(&source.contents);
            print_to(&outcome.spec, &pcfg, &mut sink);
            // `| head` / `| less q` closing the pipe is a normal
            // termination signal — stop feeding the printer further
            // inputs and exit cleanly.
            if sink.has_broken_pipe() {
                tracing::debug!(
                    path = %source.display_name(),
                    "broken pipe on stdout; downstream consumer closed early"
                );
                // Clear the latched BrokenPipe so it does not flip the
                // exit code below.
                let _ = sink.take_error();
                break;
            }
        }

        // Surface any non-BrokenPipe I/O failure that the printer
        // swallowed (the `PrintWriter` trait returns `()`).
        if let Some(e) = sink.take_error() {
            eprintln!("error writing pretty output: {e:#}");
            return Ok(ExitCode::from(2));
        }

        Ok(if any_io_error {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        })
    }
}
