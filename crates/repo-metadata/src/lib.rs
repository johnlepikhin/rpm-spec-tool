//! HTTP fetch, on-disk cache, and metadata parsers for the
//! `rpm-spec-tool` repository subsystem.
//!
//! This crate is the only one allowed to touch the network or write
//! to the on-disk cache. `rpm-spec-analyzer` consumes the resulting
//! [`rpm_spec_repo_core::RepoIndex`] / [`rpm_spec_repo_core::RepoUniverse`]
//! via the [`backend::RepoBackend`] trait without ever importing the
//! HTTP, compression, or XML dependencies.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]
#![expect(
    missing_docs,
    reason = "pre-1.0: doc backlog tracked separately; switch fires loudly when backlog reaches zero"
)]

#[cfg(not(target_os = "linux"))]
compile_error!("rpm-spec-repo-metadata requires Linux");

pub mod backend;
pub mod cache;
pub mod compression;
pub mod http;
pub mod locks;
pub(crate) mod util;

#[cfg(feature = "rpm-md")]
pub mod rpmmd;

#[cfg(feature = "apt-rpm")]
pub mod aptrpm;

#[cfg(feature = "gpg")]
pub mod gpg;
