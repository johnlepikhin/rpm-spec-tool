//! `format` subcommand — pretty-print spec files using `rpm_spec::printer`.

use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use rpm_spec::printer::{PrinterConfig, print_with};
use rpm_spec_analyzer::config::FormatConfig;
use rpm_spec_analyzer::parse;

use crate::app::ColorChoice;
use crate::config as cli_config;
use crate::io;

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
}

impl Cmd {
    pub fn run(self, _color: ColorChoice) -> Result<ExitCode> {
        let sources = io::read_sources(&self.input.paths)?;
        let mut config_cache = cli_config::ConfigCache::new(self.input.config.clone());

        let mut would_change = false;
        let mut any_io_error = false;

        for source in sources {
            let analyzer_cfg = match config_cache.load_for(&source.path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    any_io_error = true;
                    continue;
                }
            };
            let pcfg = build_printer_config(&analyzer_cfg.format, self.preamble_align_column);

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
                if changed
                    && let Err(e) = io::write_atomic(&source.path, &formatted)
                {
                    eprintln!("error writing {}: {e:#}", source.display_name());
                    any_io_error = true;
                }
            } else if self.diff {
                emit_diff(&source.display_name(), &source.contents, &formatted);
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

fn build_printer_config(cfg: &FormatConfig, column_override: Option<u32>) -> PrinterConfig {
    match column_override {
        None => cfg.to_printer_config(),
        Some(0) => cfg.to_printer_config().with_preamble_value_column(None),
        Some(col) => cfg
            .to_printer_config()
            .with_preamble_value_column(Some(col as usize)),
    }
}

fn emit_diff(name: &str, before: &str, after: &str) {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(before, after);
    println!("--- {name} (original)");
    println!("+++ {name} (formatted)");
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Equal => " ",
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
        };
        print!("{sign}{change}");
    }
}
