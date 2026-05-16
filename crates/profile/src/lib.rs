//! Distribution profiles for the RPM spec analyzer.
//!
//! A [`Profile`] is the resolved target environment a `.spec` file is
//! analyzed against — identity (family/vendor/dist tag), the full macro
//! registry, rpmlib features, license/group whitelists. Layered:
//! builtin baseline → `rpm --showrc` dump → user overrides.
//!
//! Sources of profile data:
//! 1. Builtins compiled into the binary (`data/<name>.toml`, loaded via
//!    [`builtin`]). On first release only `generic` exists.
//! 2. A `rpm --showrc` dump from the target host, parsed by [`showrc`].
//! 3. User overrides in `.rpmspec.toml` `[profiles.<name>.*]`, deserialised
//!    via [`config_layer`] and merged on top.
//!
//! Entry point: [`resolve_profile`].

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]
// TODO(pre-1.0): document the public surface and remove this allow.
// Currently 103 items lack `///` doc comments — chiefly per-rule
// structs in `rules/` and per-layer config types. Tracked separately
// from publication.
#![allow(missing_docs)]

pub mod autodetect;
pub mod builtin;
pub mod config_layer;
pub mod merge;
pub mod overrides;
pub mod resolve;
pub mod showrc;
pub mod types;

pub use config_layer::{
    ListOverride, MacroOverride, ProfileEntry, ProfileIdentityOverride, ProfileSection,
};
pub use overrides::{CliDefine, DefineParseError, parse_define};
pub use resolve::{ResolveError, ResolveOptions, resolve as resolve_profile};
pub use types::{
    ArchInfo, Family, GroupList, Identity, LayerInfo, LicenseList, MacroEntry, MacroRegistry,
    MacroValue, Profile, Provenance, RpmlibFeatures, ValidationMode,
};
