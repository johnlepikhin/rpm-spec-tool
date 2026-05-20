//! Repository-aware lint rules.
//!
//! Every rule in this module consumes a `RepoUniverse` provided via
//! [`crate::Lint::set_repo_universe`]. When the universe is `None`
//! (no repos configured for the active profile, or cache miss in
//! offline mode) every rule short-circuits — no diagnostics emitted,
//! no per-lint warning. The CLI surfaces a single one-time INFO
//! note at session setup so the user knows why the rules are quiet.
//!
//! Rule IDs use the `RPM-REPO-NNN` namespace, sorted by priority
//! family:
//!
//! | ID                 | Default | Purpose                                                            |
//! |--------------------|---------|--------------------------------------------------------------------|
//! | `RPM-REPO-001`     | deny    | `BuildRequires:` atom has no provider in any configured repo       |
//! | `RPM-REPO-002`     | warn    | `Requires:` atom has no provider in any configured repo            |
//! | `RPM-REPO-003`     | warn    | `BuildRequires:` provider exists but version constraint unmet      |
//! | `RPM-REPO-010`     | warn    | build script invokes bare command (`cmake`, `meson`, ...) with no matching `BuildRequires` |
//! | `RPM-REPO-011`     | warn    | build script uses absolute tool path with no matching `BuildRequires` |
//! | `RPM-REPO-020`     | warn    | path in `%files` is already owned by another package in the repos |
//! | `RPM-REPO-030`     | deny    | spec EVR is not strictly greater than the latest published binary |
//! | `RPM-REPO-031`     | deny    | spec drops or lowers the `Epoch:` set on the published binary |

pub mod br_unresolvable;
pub mod br_version_unsatisfied;
pub mod file_conflict;
pub mod missing_br_for_command;
pub mod missing_br_for_file;
pub mod runtime_unresolvable;
pub mod upgrade_check;

pub mod shared;

// Test fixtures (`redos_profile()` + `tiny_universe()`) are exposed
// publicly under a feature gate so the `tests/repo_lints_smoke.rs`
// integration test can share them with the unit tests in `repo/*.rs`
// and avoid the dual-maintenance trap of duplicated `tiny_universe()`
// builders that drift apart.
#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub mod test_fixtures;
