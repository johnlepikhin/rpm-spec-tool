//! Reusable analysis primitives for shell-bearing sections.
//!
//! Phase 19 introduces shared infrastructure that the new
//! scriptlet/build-script rules (RPM340 onward) reach for:
//!
//! - [`tokens`] — line-level tokenizer that splits a `Text` (literal +
//!   macro segments) into shell-word tokens, honouring single/double
//!   quoting. The project deliberately avoids a full shell AST (a
//!   later phase may revisit); the tokenizer is the minimum needed to
//!   pull out command names and arguments per line.
//! - [`walk`] — visitors over every `ShellBody`-bearing section
//!   (`%prep`, `%build`, `%install`, `%check`, `%clean`,
//!   `%generate_buildrequires`, every scriptlet, every trigger, every
//!   file trigger, `%verify`, `%sepolicy`). Centralises the AST recursion
//!   so each rule does not re-walk `SpecFile.items`.
//! - [`cmd_index`] — `CommandUseIndex` built on top of the two above:
//!   a flat list of `CommandUse { name, args, location, line_idx }`
//!   for cross-section queries ("does any scriptlet call `systemctl`?",
//!   "any `%install` line invoking `make install` without `DESTDIR`?").
//!
//! Macro references inside arguments are preserved verbatim (as
//! `ShellArg::Macro`) so future passes can resolve them; literal-only
//! arguments stay cheap `&str` slices.

pub(crate) mod cmd_index;
pub(crate) mod tokens;
pub(crate) mod walk;

// Selectively re-export the types and helpers call sites use by name.
// `CommandUse`, `ScriptKind`, and `ShellArg` are reached through method
// calls on the surfaced types, so re-exporting them at the module root
// would only add noise. `tokenize_line` is re-exported so the line-level
// scanner in `rules/parallel_make.rs` doesn't have to path-pierce into
// the private `tokens` submodule.
pub(crate) use cmd_index::{CommandUseIndex, SectionRef};
pub(crate) use tokens::{ShellToken, first_non_flag_arg, strip_trailing_comment, tokenize_line};
pub(crate) use walk::{for_each_buildscript, for_each_scriptlet};
