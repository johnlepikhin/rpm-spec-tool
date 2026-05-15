//! `ast` subcommand — dump a parsed `SpecFile<Span>` as JSON or YAML.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};
use rpm_spec_analyzer::parse;

use crate::io;

#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum DumpFormat {
    Json,
    Yaml,
}

#[derive(Debug, Args)]
pub struct Cmd {
    /// Spec file to parse. `-` (or omitted) reads stdin.
    pub path: Option<std::path::PathBuf>,

    #[arg(long, default_value_t = DumpFormat::Json, value_enum)]
    pub format: DumpFormat,

    /// Pretty-print the output.
    #[arg(long)]
    pub pretty: bool,
}

impl Cmd {
    pub fn run(self) -> Result<ExitCode> {
        let paths = self.path.into_iter().collect::<Vec<_>>();
        let sources = io::read_sources(&paths)?;
        let source = sources.into_iter().next().expect("read_sources guarantees at least one");
        let outcome = parse(&source.contents);
        match (self.format, self.pretty) {
            (DumpFormat::Json, true) => {
                println!("{}", serde_json::to_string_pretty(&outcome.spec)?)
            }
            (DumpFormat::Json, false) => println!("{}", serde_json::to_string(&outcome.spec)?),
            (DumpFormat::Yaml, _) => println!("{}", serde_yaml::to_string(&outcome.spec)?),
        }
        Ok(ExitCode::SUCCESS)
    }
}
