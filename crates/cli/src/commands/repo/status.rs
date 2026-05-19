//! `repo status` — quick health check for a profile's configured repos.
//!
//! Unlike `repo show`, this command never loads the full package list:
//! it only reads each repo's `meta` table (revision, fetched_at,
//! backend kind) plus a `COUNT(*)`. Designed for CI gates and for the
//! "did my sync actually pick up every repo?" question.
//!
//! Statuses:
//! * `OK` — `current` symlink resolves, `repo.db` opens, has a package
//!   count > 0.
//! * `EMPTY` — opens cleanly but reports zero packages. Likely a
//!   parser bug or an upstream repo that genuinely has no packages
//!   (rare).
//! * `MISSING` — no `current` snapshot directory. Run `repo sync`.
//! * `NO_DB` — snapshot exists but `repo.db` not yet written (legacy
//!   bincode-only snapshot from before the SQLite migration). Re-run
//!   `repo sync` to upgrade.
//! * `ERROR(msg)` — snapshot exists, `repo.db` exists, but opening or
//!   reading it failed. Re-sync usually fixes.
//! * `DISABLED` — `enabled = false` in profile config; skipped.
//! * `NO_BASEURL` — repo has no `baseurl` set in config.
//! * `BAD_URL(msg)` — placeholder interpolation failed (e.g. SSRF
//!   guard rejected a `$basearch` value).
//!
//! Exit code:
//! * `0` — every enabled repo with a baseurl is `OK`.
//! * `1` — at least one repo is in a non-OK, non-DISABLED state.
//! * `2` — user error (unknown profile, etc).

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::Serialize;

use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{ProfileSection, ResolveOptions, resolve_profile};
use rpm_spec_repo_core::db::RepoDb;
use rpm_spec_repo_metadata::cache::CacheDirs;

use super::{DEFAULT_PROFILE_NAME, RepoArgs};
use crate::commands::profile::style::Style;
use crate::commands::repo::url::interpolate_url;

#[derive(Debug, Args)]
pub struct StatusOpts {
    /// Profile to inspect (defaults to the active profile in
    /// `.rpmspec.toml`).
    #[arg(long = "profile", value_name = "NAME")]
    pub profile: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = StatusFormat::Human)]
    pub format: StatusFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StatusFormat {
    Human,
    Json,
}

#[derive(Debug, Serialize)]
struct StatusReport {
    profile: String,
    repos: Vec<RepoStatusRow>,
    summary: StatusSummary,
}

#[derive(Debug, Serialize)]
struct RepoStatusRow {
    repo_id: String,
    status: &'static str,
    detail: Option<String>,
    baseurl: Option<String>,
    revision: Option<String>,
    fetched_at: Option<String>,
    backend_kind: Option<String>,
    package_count: Option<u64>,
    db_path: Option<String>,
    db_size_bytes: Option<u64>,
}

#[derive(Debug, Serialize, Default)]
struct StatusSummary {
    ok: usize,
    empty: usize,
    missing: usize,
    no_db: usize,
    error: usize,
    disabled: usize,
    no_baseurl: usize,
    bad_url: usize,
}

impl StatusSummary {
    fn record(&mut self, status: &'static str) {
        match status {
            "OK" => self.ok += 1,
            "EMPTY" => self.empty += 1,
            "MISSING" => self.missing += 1,
            "NO_DB" => self.no_db += 1,
            "ERROR" => self.error += 1,
            "DISABLED" => self.disabled += 1,
            "NO_BASEURL" => self.no_baseurl += 1,
            "BAD_URL" => self.bad_url += 1,
            _ => {}
        }
    }

    /// Non-zero exit if any enabled, baseurl-carrying repo isn't OK.
    fn exit_code(&self) -> ExitCode {
        if self.missing + self.no_db + self.error + self.empty + self.bad_url == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        }
    }
}

pub fn run(
    opts: StatusOpts,
    repo_args: RepoArgs,
    config: &Config,
    base_dir: &Path,
    style: &Style,
) -> Result<ExitCode> {
    let cache_root = repo_args.resolve_cache_root()?;
    let dirs = CacheDirs::ensure(cache_root)?;

    let section = ProfileSection::new(config.profile.clone(), config.profiles.clone());
    let active = opts
        .profile
        .clone()
        .or_else(|| config.profile.clone())
        .unwrap_or_else(|| DEFAULT_PROFILE_NAME.to_string());
    let profile = resolve_profile(
        &section,
        base_dir,
        ResolveOptions::with_override(Some(active.as_str())),
    )?;

    let mut report = StatusReport {
        profile: active.clone(),
        repos: Vec::new(),
        summary: StatusSummary::default(),
    };

    let repos = match &profile.repos {
        Some(rs) => &rs.repos,
        None => {
            // No repos at all is a config-level "nothing to check".
            // Treat as success so CI doesn't fail on profiles that
            // intentionally don't pin repos.
            return emit(&report, opts.format, style, /* empty config */ true);
        }
    };

    for (repo_id, cfg) in repos {
        let mut row = RepoStatusRow {
            repo_id: repo_id.clone(),
            status: "OK",
            detail: None,
            baseurl: cfg.baseurl.clone(),
            revision: None,
            fetched_at: None,
            backend_kind: None,
            package_count: None,
            db_path: None,
            db_size_bytes: None,
        };

        if !cfg.enabled {
            row.status = "DISABLED";
            report.summary.record(row.status);
            report.repos.push(row);
            continue;
        }
        let Some(baseurl) = &cfg.baseurl else {
            row.status = "NO_BASEURL";
            report.summary.record(row.status);
            report.repos.push(row);
            continue;
        };

        let interpolated = match interpolate_url(baseurl, &profile) {
            Ok(u) => u,
            Err(e) => {
                row.status = "BAD_URL";
                row.detail = Some(e);
                report.summary.record(row.status);
                report.repos.push(row);
                continue;
            }
        };
        row.baseurl = Some(interpolated.clone());

        let repo_dir = dirs.repo_dir(&interpolated);
        let current = repo_dir.join("current");
        if !current.exists() {
            row.status = "MISSING";
            row.detail = Some("no `current` snapshot — run `repo sync --allow-fetch`".to_string());
            report.summary.record(row.status);
            report.repos.push(row);
            continue;
        }

        let db_path = current.join(RepoDb::file_name());
        row.db_path = Some(db_path.display().to_string());
        if !db_path.exists() {
            row.status = "NO_DB";
            row.detail = Some(
                "snapshot present but `repo.db` missing (pre-SQLite-migration snapshot); \
                 re-run `repo sync --allow-fetch`"
                    .to_string(),
            );
            report.summary.record(row.status);
            report.repos.push(row);
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&db_path) {
            row.db_size_bytes = Some(meta.len());
        }

        match RepoDb::open(&db_path) {
            Ok(db) => match (db.revision(), db.fetched_at(), db.meta("backend_kind"), db.package_count()) {
                (Ok(rev), Ok(ts), Ok(kind), Ok(count)) => {
                    row.revision = Some(rev);
                    row.fetched_at = Some(ts.to_string());
                    row.backend_kind = kind;
                    row.package_count = Some(count);
                    row.status = if count == 0 { "EMPTY" } else { "OK" };
                }
                (Err(e), _, _, _)
                | (_, Err(e), _, _)
                | (_, _, Err(e), _)
                | (_, _, _, Err(e)) => {
                    row.status = "ERROR";
                    row.detail = Some(e.to_string());
                }
            },
            Err(e) => {
                row.status = "ERROR";
                row.detail = Some(e.to_string());
            }
        }

        report.summary.record(row.status);
        report.repos.push(row);
    }

    emit(&report, opts.format, style, false)
}

fn emit(
    report: &StatusReport,
    format: StatusFormat,
    style: &Style,
    no_repos_configured: bool,
) -> Result<ExitCode> {
    let mut stdout = std::io::stdout().lock();
    match format {
        StatusFormat::Json => {
            serde_json::to_writer_pretty(&mut stdout, report)
                .with_context(|| "writing JSON status report")?;
            writeln!(stdout)?;
        }
        StatusFormat::Human => {
            writeln!(
                stdout,
                "{}",
                style.bold_cyan(&format!("== {} ==", report.profile))
            )?;
            if no_repos_configured {
                writeln!(stdout, "  (no repos configured for this profile)")?;
            } else if report.repos.is_empty() {
                writeln!(stdout, "  (no repos)")?;
            } else {
                for row in &report.repos {
                    let badge = badge_for(style, row.status);
                    writeln!(stdout, "  {badge}  {}", row.repo_id)?;
                    if let Some(url) = &row.baseurl {
                        writeln!(stdout, "        url:       {url}")?;
                    }
                    if let Some(rev) = &row.revision {
                        writeln!(stdout, "        revision:  {rev}")?;
                    }
                    if let Some(ts) = &row.fetched_at {
                        writeln!(stdout, "        fetched:   {ts}")?;
                    }
                    if let Some(kind) = &row.backend_kind {
                        writeln!(stdout, "        backend:   {kind}")?;
                    }
                    if let Some(count) = row.package_count {
                        writeln!(stdout, "        packages:  {count}")?;
                    }
                    if let Some(size) = row.db_size_bytes {
                        writeln!(stdout, "        db_size:   {size} bytes")?;
                    }
                    if let Some(detail) = &row.detail {
                        writeln!(stdout, "        detail:    {detail}")?;
                    }
                }
                writeln!(stdout)?;
                let s = &report.summary;
                writeln!(
                    stdout,
                    "  summary: {} ok, {} empty, {} missing, {} no-db, {} error, \
                     {} disabled, {} no-baseurl, {} bad-url",
                    s.ok, s.empty, s.missing, s.no_db, s.error, s.disabled, s.no_baseurl, s.bad_url,
                )?;
            }
        }
    }
    Ok(report.summary.exit_code())
}

fn badge_for(style: &Style, status: &str) -> String {
    match status {
        "OK" => style.bold_cyan("[ OK    ]"),
        "EMPTY" => style.bold_cyan("[EMPTY  ]"),
        "MISSING" => style.bold_cyan("[MISSING]"),
        "NO_DB" => style.bold_cyan("[NO_DB  ]"),
        "ERROR" => style.bold_cyan("[ERROR  ]"),
        "DISABLED" => style.bold_cyan("[DISABLD]"),
        "NO_BASEURL" => style.bold_cyan("[NO_URL ]"),
        "BAD_URL" => style.bold_cyan("[BADURL ]"),
        other => other.to_string(),
    }
}
