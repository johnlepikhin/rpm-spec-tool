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
// TODO(pre-1.0): document the public surface and remove this expect.
// Currently 103 items lack `///` doc comments — chiefly per-rule
// structs in `rules/` and per-layer config types. Tracked separately
// from publication.
#![expect(
    missing_docs,
    reason = "pre-1.0: 103 items lack /// — track and reduce; expect form fires loudly when the backlog reaches zero"
)]

pub mod autodetect;
pub mod builtin;
pub mod config_layer;
pub mod macro_lexer;
pub mod macro_variants;
pub mod merge;
pub mod overrides;
pub mod repos;
pub mod resolve;
pub mod showrc;
pub mod types;

pub use config_layer::{
    ListOverride, MacroOverride, ProfileEntry, ProfileIdentityOverride, ProfileSection,
    TargetEntry, TargetProfileOverride,
};
pub use macro_variants::MacroVariants;
pub use overrides::{CliDefine, DefineParseError, parse_define};
pub use resolve::{ResolveError, ResolveOptions, resolve as resolve_profile, resolve_target_set};
pub use types::{
    ArchInfo, Family, GroupList, Identity, LayerInfo, LicenseList, MacroEntry, MacroRegistry,
    MacroValue, Profile, Provenance, ResolvedTarget, ResolvedTargetSet, RpmlibFeatures,
    ValidationMode,
};
