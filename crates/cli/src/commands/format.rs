//! `format` subcommand — pretty-print spec files using `rpm_spec::printer`.

use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use rpm_spec::printer::{PrinterConfig, print_with};
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

    /// Override the preamble value alignment column (defaults to config or 16).
    #[arg(long)]
    pub preamble_align_column: Option<u32>,
}

impl Cmd {
    pub fn run(self, _color: ColorChoice) -> Result<ExitCode> {
        let sources = io::read_sources(&self.input.paths)?;

        let mut would_change = false;
        for source in sources {
            let cfg_path = self.input.config.as_deref();
            let analyzer_cfg = cli_config::load(cfg_path, Some(&source.path))?;
            let pcfg = build_printer_config(&analyzer_cfg, self.preamble_align_column);

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
                if changed {
                    std::fs::write(&source.path, &formatted)?;
                }
            } else if self.diff {
                emit_diff(&source.display_name(), &source.contents, &formatted);
            } else {
                print!("{formatted}");
            }
        }

        if self.check && would_change {
            return Ok(ExitCode::from(1));
        }
        Ok(ExitCode::SUCCESS)
    }
}

fn build_printer_config(
    cfg: &rpm_spec_analyzer::config::Config,
    column_override: Option<u32>,
) -> PrinterConfig {
    let column = column_override.unwrap_or(cfg.format.preamble_align_column);
    let preamble_column = if column == 0 { None } else { Some(column as usize) };
    PrinterConfig::new()
        .with_indent(cfg.format.conditional_indent as usize)
        .with_preamble_value_column(preamble_column)
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
