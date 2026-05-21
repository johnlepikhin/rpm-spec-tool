//! `matrix expand` — per-profile annotated view of the spec source.
//!
//! For every member profile of a target set, prints the spec lines
//! verbatim with each branch directive line (`%if` / `%elif` /
//! `%else` / `%ifarch` / `%ifnarch` / `%ifos` / `%ifnos`) tagged
//! `[ACTIVE]` / `[INACTIVE]` / `[INDETERMINATE]` according to how
//! the branch evaluator resolves the condition on that profile. The output is a static analogue of `rpmspec -P`:
//! macros are NOT expanded and inactive branch bodies stay in place
//! (just visibly marked so the reader can scan past them).
//!
//! Single-spec command — `matrix expand` on a 200-line spec across
//! 10 profiles already produces 2k lines, batching would drown the
//! signal.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{CoverageReport, EvalError, session::parse};
use serde::Serialize;

use super::coverage_style::Style;
use crate::app::ColorChoice;
use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Annotated original source per profile.
    Human,
    /// Structured JSON: per-profile list of branch directive lines
    /// with their status. The full source is NOT serialised — JSON
    /// consumers can re-read the input themselves.
    Json,
}

/// `--target-set NAME` and `--profiles a,b,c` are exclusive, matching
/// the rest of the `matrix` family.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
pub struct ExpandOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    #[arg(long = "target-set", value_name = "NAME")]
    pub target_set: Option<String>,

    #[arg(long = "profiles", value_name = "P1,P2,...", value_delimiter = ',')]
    pub profiles: Vec<String>,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,

    #[command(flatten)]
    pub bcond: crate::app::BcondOverridesArg,
}

pub(super) fn run(
    opts: ExpandOpts,
    config_override: Option<&Path>,
    color: ColorChoice,
) -> Result<ExitCode> {
    let ctx = match super::prepare_matrix(
        config_override,
        opts.target_set.as_deref(),
        &opts.profiles,
        &opts.defines,
    ) {
        Ok(c) => c,
        Err(e) => return e.into_exit(),
    };
    let resolved = ctx.resolved;

    let sources = io::read_sources(&opts.input.paths)?;
    if sources.len() > 1 {
        eprintln!(
            "error: matrix expand operates on exactly one spec at a time \
             (got {} sources)",
            sources.len()
        );
        return Ok(ExitCode::from(2));
    }
    let source = sources
        .into_iter()
        .next()
        .expect("io::read_sources guarantees >= 1 source");

    let parsed = parse(&source.contents);
    // Surface parser-level issues: the per-profile annotation is
    // computed against the recovered AST, so a partial parse can
    // make a branch invisible (no tag) which would otherwise look
    // like a renderer bug.
    let display_name = source.display_name();
    super::surface_parser_diagnostics(
        super::ParseDiagnosticContext::Expand {
            display_name: &display_name,
        },
        &parsed,
    );
    let coverage = CoverageReport::compute(&parsed.spec, &resolved, &opts.bcond.to_overrides());

    match opts.format {
        OutputFormat::Human => {
            let style = Style::new(color);
            render_human(&source, &coverage, &resolved, &style)?;
        }
        OutputFormat::Json => render_json(&source, &coverage, &resolved)?,
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Per-profile branch status lookup
// ---------------------------------------------------------------------------

/// One branch's per-profile activity, indexed by source line number.
/// The key is the branch directive line (`%if`/`%elif`/`%else`/etc.);
/// the value carries the original directive text and the activity
/// verdict on the profile under consideration.
struct BranchInfo<'a> {
    directive: &'a str,
    status: BranchStatus<'a>,
}

#[derive(Debug)]
enum BranchStatus<'a> {
    Active,
    Inactive,
    /// Carries the human-readable reason from the evaluator.
    Indeterminate(&'a EvalError),
    /// Effectively suppressed by an ancestor `%if`/`%elif`/`%else`
    /// branch whose own status is `Inactive`. The line's local
    /// condition is moot because the surrounding block is skipped
    /// at build time regardless. Distinct from a plain `Inactive`
    /// so the operator can tell "this branch is dead because its
    /// own predicate failed" from "this branch is dead because a
    /// parent's predicate failed". Never appears in the coverage
    /// index — purely a render-time derivation.
    NestedInactive,
}

impl BranchStatus<'_> {
    /// Tag rendered through the colour painter. Returns an owned
    /// `String` because the styled forms always carry ANSI
    /// escapes (or are identity-mapped when colour is disabled,
    /// matching plain `tag()` byte-for-byte).
    ///
    /// Palette:
    /// * green `[ACTIVE]` — the branch fires on this profile.
    /// * orange `[INACTIVE]` — branch skipped by its own condition
    ///   (xterm-256 index 208); distinct from green and red at a
    ///   glance.
    /// * dim orange `[INACTIVE: nested]` — branch suppressed by an
    ///   inactive ancestor; the local condition didn't decide.
    /// * red `[INDETERMINATE: …]` — evaluator couldn't decide;
    ///   reuses the bold-red family ("needs operator attention")
    ///   shared with `matrix coverage`'s `[DEAD]` and with
    ///   portability's `missing` count.
    fn tag_styled(&self, style: &Style) -> String {
        match self {
            Self::Active => style.always_tag("[ACTIVE]"),
            Self::Inactive => style.inactive_tag("[INACTIVE]"),
            Self::NestedInactive => style.nested_inactive_tag("[INACTIVE: nested]"),
            Self::Indeterminate(reason) => {
                style.indeterminate_tag(&format!("[INDETERMINATE: {reason}]"))
            }
        }
    }

    fn kind_label(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::NestedInactive => "nested-inactive",
            Self::Indeterminate(_) => "indeterminate",
        }
    }
}

/// Build `line → BranchInfo` for one profile. Returns an empty map
/// if the coverage report has no conditionals (spec has no `%if`).
fn build_profile_index<'a>(
    coverage: &'a CoverageReport,
    profile_id: &str,
) -> HashMap<u32, BranchInfo<'a>> {
    let mut out = HashMap::new();
    for cond in &coverage.conditionals {
        for b in &cond.branches {
            let status = if b.active_on.iter().any(|p| p == profile_id) {
                BranchStatus::Active
            } else if b.inactive_on.iter().any(|p| p == profile_id) {
                BranchStatus::Inactive
            } else if b.indeterminate_on.iter().any(|p| p == profile_id) {
                let reason = b
                    .indeterminate_reasons
                    .get(profile_id)
                    .expect("indeterminate_on guarantees a matching reasons entry");
                BranchStatus::Indeterminate(reason)
            } else {
                // Defensive: a branch might exist in the coverage
                // report without the profile appearing in any list
                // (e.g. a future evaluator state). Skip rather than
                // render a misleading "ACTIVE" by default.
                continue;
            };
            out.insert(
                b.branch.span.start_line,
                BranchInfo {
                    directive: b.branch.display.as_str(),
                    status,
                },
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn render_human(
    source: &io::Source,
    coverage: &CoverageReport,
    target_set: &ResolvedTargetSet,
    style: &Style,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{}",
        style.header(&format!(
            "Matrix expand: target set `{}` ({} profiles)",
            target_set.id,
            target_set.targets.len()
        ))
    )?;
    writeln!(out, "{} {}", style.header("==>"), source.display_name())?;

    for rt in &target_set.targets {
        let index = build_profile_index(coverage, &rt.profile_id);
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            style.header(&format!("== Profile {} ==", rt.profile_id))
        )?;
        // Walk the source line-by-line, applying pretty-style
        // conditional indentation (mirrors `rpm-spec-tool pretty`
        // with `--conditional-indent=2`). The full pretty-printer
        // would re-flow other formatting too (e.g. `%global NAME
        // multiple    spaces` → single space), but `matrix expand`
        // keeps line-numbers aligned with the coverage report's
        // `Span::start_line`, so we apply ONLY the indent pass —
        // re-routing through the pretty-printer would invalidate
        // the per-line tag lookup.
        //
        // Per-conditional stack tracks the statuses of `%if` /
        // `%elif` branches seen so far, so when `%else` arrives we
        // can compute its activity as the complement of its
        // siblings — the AST stores `%else` separately under
        // `Conditional.otherwise` without a span, so the coverage
        // report's per-line index doesn't carry it.
        //
        // Each frame also tracks the status of the branch we are
        // currently inside (`current_branch_status`). Used to
        // detect "ancestor inactive" suppression: if any ancestor
        // frame's currently-entered branch is `Inactive`, every
        // nested branch (regardless of its own predicate) is
        // displayed as `[INACTIVE: nested]` — the parent block
        // doesn't execute, so the inner verdict is moot.
        let mut depth: usize = 0;
        let mut cond_stack: Vec<CondFrame<'_>> = Vec::new();
        for (line_no, line) in source.contents.lines().enumerate() {
            let line_no = (line_no + 1) as u32;
            let kind = classify_line(line);
            let render_depth = match kind {
                ConditionalKind::Open | ConditionalKind::None => depth,
                ConditionalKind::ElseOrElif => depth.saturating_sub(1),
                ConditionalKind::Close => depth.saturating_sub(1),
            };
            let indent = " ".repeat(render_depth * EXPAND_CONDITIONAL_INDENT);
            // Strip the source line's existing leading whitespace
            // so re-indentation is idempotent.
            let body = line.trim_start();

            // Check "ancestor inactive" *before* mutating the stack
            // for this line. For an `%if` Open we look at strictly
            // outer frames; for `%elif`/`%else` and inner body the
            // top frame represents the conditional whose siblings we
            // belong to, so its OWN current-branch status is the
            // sibling-status we're about to update — outer frames
            // are what suppresses us.
            let ancestor_inactive = ancestor_inactive(&cond_stack, kind);

            // Resolve the tag for this line:
            //  * `%if`-family directives: look up in the
            //    coverage-driven per-line index.
            //  * `%else`: derive from the siblings collected so
            //    far in `cond_stack.last()`.
            //  * Everything else: untagged.
            let mut tagged: Option<BranchStatus<'_>> = None;
            if matches!(kind, ConditionalKind::Open | ConditionalKind::ElseOrElif) {
                if body.starts_with("%else") && !body.starts_with("%elif") {
                    // Only tag `%else` if we actually know its
                    // siblings' statuses. Empty siblings → parent
                    // `%if` wasn't in the coverage report. Stay
                    // silent — internally consistent with the
                    // untagged `%if` above.
                    let mut updated = false;
                    if let Some(top) = cond_stack.last_mut()
                        && !top.siblings.is_empty()
                    {
                        let complement = complement_status(&top.siblings);
                        top.current_branch_status = Some(borrow_status(&complement));
                        tagged = Some(complement);
                        updated = true;
                    }
                    if updated {
                        refresh_inactive_chain(&mut cond_stack);
                    }
                } else if let Some(info) = index.get(&line_no) {
                    // Clone the borrowed status into our local
                    // tag slot. `BranchStatus<'a>` borrows the
                    // `EvalError` from `coverage`; we only need
                    // it to compute the printed string, so this
                    // is purely a borrow-flow convenience.
                    tagged = Some(borrow_status(&info.status));
                    // Track the `%if`/`%elif` status for the
                    // eventual `%else` complement AND for nested
                    // suppression detection.
                    let mut updated = false;
                    if let Some(top) = cond_stack.last_mut()
                        && matches!(kind, ConditionalKind::ElseOrElif)
                    {
                        top.siblings.push(borrow_status(&info.status));
                        top.current_branch_status = Some(borrow_status(&info.status));
                        updated = true;
                    }
                    if updated {
                        refresh_inactive_chain(&mut cond_stack);
                    }
                }
                if matches!(kind, ConditionalKind::Open) {
                    // Push a fresh frame for the new conditional.
                    // Seed siblings with the `%if` head's status so
                    // the eventual `%else` sees it.
                    let seed = index.get(&line_no).map(|i| borrow_status(&i.status));
                    let current = seed.as_ref().map(borrow_status);
                    let inactive_chain = compute_inactive_chain(cond_stack.last(), &current);
                    cond_stack.push(CondFrame {
                        siblings: seed.into_iter().collect(),
                        current_branch_status: current,
                        inactive_chain,
                    });
                }
            }

            // Apply ancestor-inactive suppression. We only override
            // when the line has a tag AND that tag is *not already*
            // `Inactive` (own predicate already says "skip", so the
            // ancestor-reason annotation would be redundant noise).
            if ancestor_inactive {
                tagged = match tagged {
                    Some(BranchStatus::Inactive) => Some(BranchStatus::Inactive),
                    Some(_) => Some(BranchStatus::NestedInactive),
                    None => None,
                };
            }

            match tagged {
                Some(status) => {
                    writeln!(out, "{indent}{body}  {}", status.tag_styled(style))?;
                }
                None => writeln!(out, "{indent}{body}")?,
            }

            // Update depth + pop on close AFTER emitting.
            match kind {
                ConditionalKind::Open => depth += 1,
                ConditionalKind::Close => {
                    depth = depth.saturating_sub(1);
                    cond_stack.pop();
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// One open `%if`...`%endif` block on the per-profile render stack.
///
/// `siblings` accumulates `%if`/`%elif` head statuses (for `%else`
/// complement); `current_branch_status` mirrors the *most recently
/// entered* branch's status so nested conditionals can detect when an
/// outer branch is inactive and downgrade their tags accordingly.
///
/// `inactive_chain` is an O(1) cache of "is any strictly-outer
/// frame's currently-entered branch `Inactive`?" computed at push
/// time and refreshed whenever the frame's `current_branch_status`
/// changes (i.e. on every `%elif`/`%else`). With this field the
/// ancestor check is a single bool read per source line instead of
/// scanning the whole stack — depth is small in practice (≤5) but
/// the cost is paid on every line, including the ~10k-line monolith
/// specs from kernel/firmware packages, which adds up.
struct CondFrame<'a> {
    siblings: Vec<BranchStatus<'a>>,
    current_branch_status: Option<BranchStatus<'a>>,
    /// `true` when this frame OR any of its ancestors has an
    /// `Inactive` `current_branch_status`. Read by [`ancestor_inactive`]
    /// to decide whether to downgrade tags to `NestedInactive`.
    inactive_chain: bool,
}

/// Returns `true` when any strictly-outer frame's currently-entered
/// branch is `Inactive`. For an `%if` Open line the "strictly outer"
/// is the whole existing stack (the new frame for THIS `%if` isn't
/// pushed yet); for `%elif`/`%else` and inner body lines the top
/// frame IS the conditional we belong to, so we still want only
/// strictly-outer ancestors — but on body lines the top frame's
/// `current_branch_status` is OUR branch, not a parent, so we must
/// also exclude it.
///
/// Easiest invariant: ancestor = "all frames except the *innermost
/// one that owns the line we're rendering*". For `%if` Open the
/// owning frame doesn't exist yet, so all current stack entries are
/// ancestors. For all other line kinds (body, `%elif`, `%else`,
/// `%endif`) the owning frame is `stack.last()`, so ancestors are
/// `stack[..stack.len()-1]`.
///
/// Implementation note: `CondFrame::inactive_chain` is maintained as
/// a running OR of "this frame Inactive" with the parent's chain, so
/// the answer is an O(1) read of the relevant frame's flag rather
/// than an O(depth) scan per source line.
fn ancestor_inactive(stack: &[CondFrame<'_>], kind: ConditionalKind) -> bool {
    match kind {
        // `%if` Open: the frame for THIS conditional isn't on the
        // stack yet, so the answer is whatever the current top
        // frame says — its `inactive_chain` already folds in all
        // strictly-outer ancestors' verdicts plus its own.
        ConditionalKind::Open => stack.last().is_some_and(|f| f.inactive_chain),
        // For body lines, `%elif`/`%else`, and `%endif` close, the
        // top frame is the conditional we belong to — its
        // `current_branch_status` is our branch's own verdict, not
        // a parent's. Look one frame up (its `inactive_chain` does
        // NOT include the top frame's own branch).
        _ => stack
            .len()
            .checked_sub(2)
            .and_then(|idx| stack.get(idx))
            .is_some_and(|f| f.inactive_chain),
    }
}

/// Compute `inactive_chain` for a fresh frame given the parent
/// (innermost frame on the existing stack, if any) and the frame's
/// own initial `current_branch_status`.
fn compute_inactive_chain(parent: Option<&CondFrame<'_>>, own: &Option<BranchStatus<'_>>) -> bool {
    let parent_inactive = parent.is_some_and(|f| f.inactive_chain);
    let own_inactive = matches!(own, Some(BranchStatus::Inactive));
    parent_inactive || own_inactive
}

/// Re-derive a frame's `inactive_chain` after `current_branch_status`
/// changes (e.g. moving from `%if` head into an `%elif` sibling).
/// The parent's flag is stable inside one open frame, so this only
/// flips when the frame's own branch status flips between Inactive
/// and not-Inactive.
fn refresh_inactive_chain(stack: &mut [CondFrame<'_>]) {
    let len = stack.len();
    if len == 0 {
        return;
    }
    // Split so we can borrow the parent (`..len-1`) immutably while
    // mutating the top frame.
    let parent_inactive = if len >= 2 {
        stack[len - 2].inactive_chain
    } else {
        false
    };
    let top = &mut stack[len - 1];
    let own_inactive = matches!(top.current_branch_status, Some(BranchStatus::Inactive));
    top.inactive_chain = parent_inactive || own_inactive;
}

/// Re-borrow a `BranchStatus` so the caller can collect statuses
/// into a `Vec` without owning the underlying `EvalError`. Trivial
/// for unit variants; for `Indeterminate` the `&EvalError` is
/// copied as a reference (cheap, same lifetime).
fn borrow_status<'a>(s: &BranchStatus<'a>) -> BranchStatus<'a> {
    match s {
        BranchStatus::Active => BranchStatus::Active,
        BranchStatus::Inactive => BranchStatus::Inactive,
        BranchStatus::NestedInactive => BranchStatus::NestedInactive,
        BranchStatus::Indeterminate(r) => BranchStatus::Indeterminate(r),
    }
}

/// Compute the activity of an implicit `%else` from its sibling
/// `%if`/`%elif` branches. Semantics mirror RPM's evaluation:
///
/// * any sibling active → `%else` is **inactive** (the spec already
///   committed to a non-`else` branch on this profile);
/// * all siblings inactive → `%else` is **active** (its body is
///   what runs);
/// * any sibling indeterminate AND none active → `%else` is
///   **indeterminate** (we can't decide whether one of the
///   ambiguous siblings would have fired, so the else's reach is
///   unknown too).
fn complement_status<'a>(siblings: &[BranchStatus<'a>]) -> BranchStatus<'a> {
    let mut indet: Option<&BranchStatus<'a>> = None;
    for s in siblings {
        match s {
            BranchStatus::Active => return BranchStatus::Inactive,
            #[allow(clippy::collapsible_match)]
            BranchStatus::Indeterminate(_) => {
                if indet.is_none() {
                    indet = Some(s);
                }
            }
            BranchStatus::Inactive | BranchStatus::NestedInactive => {}
        }
    }
    match indet {
        Some(BranchStatus::Indeterminate(r)) => BranchStatus::Indeterminate(r),
        _ => BranchStatus::Active,
    }
}

/// Spaces inserted per `%if` nesting level. Matches the
/// `[format] conditional-indent = 2` default of the pretty-printer
/// — keep the constant in sync if the printer's default changes.
const EXPAND_CONDITIONAL_INDENT: usize = 2;

/// Classification of a source line by its leading directive (if any).
/// Drives `render_human`'s indent state machine: openings increment
/// the depth, closings decrement, else/elif sit at one level shallower.
#[derive(Debug, Clone, Copy)]
enum ConditionalKind {
    /// `%if` / `%ifarch` / `%ifnarch` / `%ifos` / `%ifnos` —
    /// opens a new conditional block.
    Open,
    /// `%else` / `%elif` / `%elifarch` / `%elifos` — sibling
    /// branch, doesn't change depth.
    ElseOrElif,
    /// `%endif` — closes the innermost conditional.
    Close,
    /// Any non-directive line.
    None,
}

fn classify_line(line: &str) -> ConditionalKind {
    let trimmed = line.trim_start();
    // Match the longest token first (`%elifarch` before `%elif`,
    // `%ifarch` before `%if`) so the prefix check doesn't false-
    // match the shorter form.
    if trimmed.starts_with("%ifarch")
        || trimmed.starts_with("%ifnarch")
        || trimmed.starts_with("%ifos")
        || trimmed.starts_with("%ifnos")
        || (trimmed.starts_with("%if")
            && trimmed
                .as_bytes()
                .get(3)
                .is_some_and(|b| b.is_ascii_whitespace()))
    {
        ConditionalKind::Open
    } else if trimmed.starts_with("%elifarch")
        || trimmed.starts_with("%elifos")
        || trimmed.starts_with("%elif")
        || trimmed.starts_with("%else")
    {
        ConditionalKind::ElseOrElif
    } else if trimmed.starts_with("%endif") {
        ConditionalKind::Close
    } else {
        ConditionalKind::None
    }
}

#[derive(Debug, Serialize)]
struct ExpandJson<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    path: String,
    per_profile: Vec<ProfileJson<'a>>,
}

#[derive(Debug, Serialize)]
struct ProfileJson<'a> {
    profile_id: &'a str,
    branches: Vec<BranchJson<'a>>,
}

#[derive(Debug, Serialize)]
struct BranchJson<'a> {
    line: u32,
    directive: &'a str,
    /// `"active"` / `"inactive"` / `"indeterminate"` — `snake_case`
    /// matching the rest of the matrix JSON wire format.
    status: &'static str,
    /// Non-null only when `status == "indeterminate"`; carries the
    /// evaluator's Display string for the [`EvalError`].
    indeterminate_reason: Option<String>,
}

fn render_json(
    source: &io::Source,
    coverage: &CoverageReport,
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    let mut per_profile = Vec::with_capacity(target_set.targets.len());
    for rt in &target_set.targets {
        let index = build_profile_index(coverage, &rt.profile_id);
        // Walk the index in line order so the JSON output is
        // deterministic and tools can diff two runs.
        let mut entries: Vec<(u32, &BranchInfo<'_>)> = index.iter().map(|(k, v)| (*k, v)).collect();
        entries.sort_by_key(|(line, _)| *line);
        let branches: Vec<BranchJson<'_>> = entries
            .iter()
            .map(|(line, info)| BranchJson {
                line: *line,
                directive: info.directive,
                status: info.status.kind_label(),
                indeterminate_reason: match info.status {
                    BranchStatus::Indeterminate(reason) => Some(reason.to_string()),
                    _ => None,
                },
            })
            .collect();
        per_profile.push(ProfileJson {
            profile_id: rt.profile_id.as_str(),
            branches,
        });
    }
    let payload = ExpandJson {
        target_set: target_set.id.as_str(),
        profiles: target_set
            .targets
            .iter()
            .map(|t| t.profile_id.as_str())
            .collect(),
        path: source.display_name().to_string(),
        per_profile,
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &payload)?;
    use std::io::Write;
    writeln!(out)?;
    Ok(())
}
