//! Language Server Protocol implementation for RPM `.spec` files.
//!
//! Wraps `rpm-spec-analyzer` behind LSP so editors can show its
//! diagnostics inline and apply its `Suggestion`-based fixes as
//! quick actions.
//!
//! The library surface is intentionally small — most consumers want
//! the `rpm-spec-lsp` binary. The library is exposed only so the
//! integration test suite can spawn the server in-process via
//! [`lsp_server::Connection::memory`].

#![forbid(unsafe_code)]
// `lsp_types::Uri` wraps `fluent_uri::Uri<String>`, which carries an
// internal `Cell<NonZero<u32>>` for caching parsed offsets. Clippy flags
// any container keyed by it. The cell is intentionally invisible to
// equality and hashing (both go through `as_str()`), so the lint is a
// false positive here.
#![allow(clippy::mutable_key_type)]

pub mod code_actions;
pub mod completion;
pub mod diagnostics;
pub mod document;
pub mod encoding;
pub mod folding;
pub mod hover;
pub mod inlay;
pub mod outline;
pub mod rename;
pub mod server;
pub mod xref;

pub use server::Server;
