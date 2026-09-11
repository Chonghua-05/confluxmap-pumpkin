//! Runtime configuration, sourced from the plugin's WASI environment.
//!
//! Pumpkin's plugin API exposes no world-seed accessor, and the WASI sandbox
//! denies reads of `pumpkin.toml` and the world save. The seed therefore has to
//! be handed in from outside the sandbox through
//! `[plugins.overrides.<name>.environment]` in `pumpkin.toml`:
//!
//! ```toml
//! [plugins.overrides.confluxmap-pumpkin.environment]
//! CFM_SEED = "<the server's own seed>"
//! ```
//!
//! Everything except the seed is optional and has a working default, so a
//! server that only sets `CFM_SEED` still gets a complete handshake.

use std::fmt::Write as _;

use crate::protocol::{Budgets, Dim};

/// The world seed. Required for the plugin to do anything useful.
pub const ENV_SEED: &str = "CFM_SEED";
/// Overrides the worldgen version advertised to clients.
///
/// Normally this is *derived* from the running server's own version string, so
/// the override exists only for unusual setups (a fork whose version string
/// does not encode the Minecraft version).
pub const ENV_WORLDGEN: &str = "CFM_WORLDGEN";
/// Overrides the client-side cache namespace.
pub const ENV_WORLD_ID: &str = "CFM_WORLD_ID";
/// Set to `false` to advertise a policy without granting the seed.
pub const ENV_SHARE_SEED: &str = "CFM_SHARE_SEED";
/// Comma-separated dimension ids to advertise, e.g.
/// `minecraft:overworld,minecraft:the_nether`.
pub const ENV_DIMS: &str = "CFM_DIMS";

/// Minecraft version baked into the `pumpkin-plugin-api` build this plugin was
/// compiled against. Used only when the running server's version string cannot
/// be parsed, and as the cross-check baseline for [`Config::worldgen_override`].
pub const FALLBACK_MC_VERSION: &str = "26.2";

/// The vanilla dimensions whose generator cubiomes can model.
const VANILLA_DIMS: [(&str, &str); 3] = [
    ("minecraft:overworld", "overworld"),
    ("minecraft:the_nether", "the_nether"),
    ("minecraft:the_end", "the_end"),
];

/// One dimension to advertise, before the seed is attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DimSpec {
    /// Stringified dimension id, e.g. `minecraft:overworld`.
    pub id: String,
    /// Dimension type, e.g. `overworld`.
    pub kind: String,
    /// Whether prediction is possible for this dimension's generator.
    pub predictable: bool,
}

/// Resolved plugin configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The world seed, or `None` when `CFM_SEED` is absent or unparseable.
    pub seed: Option<i64>,
    /// Whether the seed may be granted to clients at all (`CFM_SHARE_SEED`).
    pub share_seed: bool,
    /// Explicit `CFM_WORLDGEN`, when set.
    pub worldgen_override: Option<String>,
    /// Explicit `CFM_WORLD_ID`, when set.
    pub world_id_override: Option<String>,
    /// Dimensions to advertise.
    pub dims: Vec<DimSpec>,
    /// Advertised rate/batch limits.
    pub budgets: Budgets,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            seed: None,
            share_seed: true,
            worldgen_override: None,
            world_id_override: None,
            dims: default_dims(),
            budgets: Budgets::default(),
        }
    }
}

impl Config {
    /// Reads the configuration from the process environment.
    ///
    /// A missing `CFM_SEED` is not an error here; the caller decides how loud to
    /// be about it (the plugin still answers the handshake so the client is told
    /// "no seed" explicitly rather than being left waiting).
    pub fn from_env() -> Self {
        Config {
            seed: env_var(ENV_SEED).as_deref().and_then(parse_seed),
            share_seed: env_var(ENV_SHARE_SEED).as_deref().is_none_or(parse_bool),
            worldgen_override: env_var(ENV_WORLDGEN).filter(|s| !s.is_empty()),
            world_id_override: env_var(ENV_WORLD_ID).filter(|s| !s.is_empty()),
            dims: env_var(ENV_DIMS)
                .as_deref()
                .map_or_else(default_dims, parse_dims),
            budgets: Budgets::default(),
        }
    }

    /// Whether the seed will actually be granted in the policy.
    pub fn grants_seed(&self) -> bool {
        self.share_seed && self.seed.is_some()
    }

    /// The seed to advertise; zero when none is available or sharing is off.
    pub fn advertised_seed(&self) -> i64 {
        if self.grants_seed() {
            self.seed.unwrap_or(0)
        } else {
            0
        }
    }

    /// A stable namespace for the client's on-disk caches.
    ///
    /// The reference Paper companion persists a random UUID per world root. The
    /// client only ever uses this as a cache directory key and never parses it,
    /// so deriving it from the seed is equally valid and needs no persistence.
    pub fn world_id(&self) -> String {
        if let Some(explicit) = &self.world_id_override {
            return explicit.clone();
        }
        match self.seed {
            Some(seed) => world_id_for(seed),
            // No seed at all: fall back to an all-zero namespace rather than
            // inventing one, so the client's cache stays obviously unseeded.
            None => "00000000-0000-0000-0000-000000000000".to_string(),
        }
    }

    /// Resolves the worldgen version handed to the client.
    ///
    /// Order of precedence:
    /// 1. `CFM_WORLDGEN`, for setups where the server version string is unusable.
    /// 2. The Minecraft version parsed out of the running server's own
    ///    `pumpkin-version` (e.g. `0.1.0-dev+26.2-26.45` -> `26.2`).
    /// 3. [`FALLBACK_MC_VERSION`], the version this plugin was built against.
    ///
    /// This value is *load bearing*: the client feeds it to cubiomes to pick
    /// generation parameters, so a wrong value produces a predicted map that
    /// disagrees with the real terrain.
    pub fn resolve_worldgen(&self, server_version: Option<&str>) -> String {
        if let Some(explicit) = &self.worldgen_override {
            return explicit.clone();
        }
        if let Some(derived) = server_version.and_then(parse_mc_version) {
            return derived;
        }
        FALLBACK_MC_VERSION.to_string()
    }

    /// Builds the per-dimension descriptors for the policy, attaching the seed
    /// where the policy actually grants it.
    pub fn dims_for_policy(&self) -> Vec<Dim> {
        let granted = self.grants_seed();
        let seed = self.advertised_seed();
        self.dims
            .iter()
            .map(|spec| Dim {
                id: spec.id.clone(),
                kind: spec.kind.clone(),
                predictable: spec.predictable,
                has_seed: granted,
                seed: if granted { seed } else { 0 },
                preset: 0,
            })
            .collect()
    }

    /// A multi-line human-readable dump for `/cfm status`.
    pub fn describe(&self, server_version: Option<&str>) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "seed           = {}",
            self.seed
                .map_or_else(|| "<unset>".to_string(), |s| s.to_string())
        );
        let _ = writeln!(out, "share_seed     = {}", self.share_seed);
        let _ = writeln!(out, "grants_seed    = {}", self.grants_seed());
        let _ = writeln!(out, "world_id       = {}", self.world_id());
        let _ = writeln!(
            out,
            "worldgen       = {} (source: {})",
            self.resolve_worldgen(server_version),
            self.worldgen_source(server_version)
        );
        let _ = writeln!(
            out,
            "server_version = {}",
            server_version.unwrap_or("<unknown>")
        );
        let dims: Vec<String> = self
            .dims
            .iter()
            .map(|d| format!("{} ({}, predictable={})", d.id, d.kind, d.predictable))
            .collect();
        let _ = writeln!(out, "dims           = {}", dims.join(", "));
        out
    }

    fn worldgen_source(&self, server_version: Option<&str>) -> &'static str {
        if self.worldgen_override.is_some() {
            "CFM_WORLDGEN override"
        } else if server_version.and_then(parse_mc_version).is_some() {
            "derived from server version"
        } else {
            "compile-time fallback"
        }
    }
}

/// Default dimension set: the overworld alone.
fn default_dims() -> Vec<DimSpec> {
    vec![spec_for("minecraft:overworld")]
}

/// Parses `CFM_DIMS`: comma-separated dimension ids. Blank entries are ignored;
/// an empty result falls back to the default so the policy is never dim-less.
fn parse_dims(raw: &str) -> Vec<DimSpec> {
    let specs: Vec<DimSpec> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(spec_for)
        .collect();
    if specs.is_empty() {
        default_dims()
    } else {
        specs
    }
}

/// Infers the dimension type from its id, defaulting `predictable` accordingly.
fn spec_for(id: &str) -> DimSpec {
    let known = VANILLA_DIMS.iter().find(|(dim_id, _)| *dim_id == id);
    match known {
        Some((_, kind)) => DimSpec {
            id: id.to_string(),
            kind: (*kind).to_string(),
            predictable: true,
        },
        None => DimSpec {
            id: id.to_string(),
            // Best effort for custom dimensions: the namespace-stripped id.
            kind: id.rsplit(':').next().unwrap_or(id).to_string(),
            // Unknown generators (datapack noise settings, modded) cannot be
            // modelled by cubiomes, so do not claim they are predictable.
            predictable: false,
        },
    }
}

/// Derives the cache namespace from the seed: `00000000-0000-0000-0000-<low48>`.
fn world_id_for(seed: i64) -> String {
    format!(
        "00000000-0000-0000-0000-{:012x}",
        (seed as u64) & 0x0000_ffff_ffff_ffff
    )
}

/// Extracts the Minecraft version from a Pumpkin version string.
///
/// Pumpkin reports e.g. `0.1.0-dev+26.2-26.45`, where the part after `+` up to
/// the first `-` is the Minecraft version.
fn parse_mc_version(pumpkin_version: &str) -> Option<String> {
    let after_plus = pumpkin_version.split_once('+')?.1;
    let mc = after_plus.split('-').next()?.trim();
    if mc.is_empty() {
        None
    } else {
        Some(mc.to_string())
    }
}

/// Reads an environment variable, treating blank as absent.
fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Parses a seed, accepting both signed decimal and a raw `u64` bit pattern (so
/// a seed above `i64::MAX` can be written the way Minecraft displays it).
fn parse_seed(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if let Ok(v) = trimmed.parse::<i64>() {
        return Some(v);
    }
    trimmed.parse::<u64>().ok().map(|v| v as i64)
}

/// Parses a boolean flag: `1/true/yes/on` and `0/false/no/off`.
fn parse_bool(raw: &str) -> bool {
    !matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_pumpkin_version_format() {
        assert_eq!(
            parse_mc_version("0.1.0-dev+26.2-26.45").as_deref(),
            Some("26.2")
        );
        assert_eq!(parse_mc_version("1.2.3+1.21.4").as_deref(), Some("1.21.4"));
        assert_eq!(parse_mc_version("26.2"), None);
        assert_eq!(parse_mc_version("0.1.0-dev+"), None);
    }

    #[test]
    fn derives_the_world_id_from_the_low_48_bits() {
        // The same derivation the reference server applies to a seed.
        assert_eq!(
            world_id_for(0x0123_4567_89AB_CDEF),
            "00000000-0000-0000-0000-456789abcdef"
        );
    }

    #[test]
    fn accepts_signed_and_unsigned_seed_forms() {
        assert_eq!(parse_seed("123"), Some(123));
        assert_eq!(parse_seed(" -7 "), Some(-7));
        assert_eq!(parse_seed("18446744073709551615"), Some(-1));
        assert_eq!(parse_seed("not-a-number"), None);
    }

    #[test]
    fn worldgen_precedence_is_override_then_server_then_fallback() {
        let mut config = Config::default();
        assert_eq!(
            config.resolve_worldgen(Some("0.1.0-dev+26.2-26.45")),
            "26.2"
        );
        assert_eq!(config.resolve_worldgen(None), FALLBACK_MC_VERSION);

        config.worldgen_override = Some("1.21.4".to_string());
        assert_eq!(
            config.resolve_worldgen(Some("0.1.0-dev+26.2-26.45")),
            "1.21.4"
        );
    }

    #[test]
    fn withholding_the_seed_zeroes_every_dim() {
        let config = Config {
            seed: Some(42),
            share_seed: false,
            ..Config::default()
        };
        assert!(!config.grants_seed());
        assert_eq!(config.advertised_seed(), 0);
        for dim in config.dims_for_policy() {
            assert!(!dim.has_seed);
            assert_eq!(dim.seed, 0);
        }
    }

    #[test]
    fn granting_the_seed_attaches_it_to_every_dim() {
        let config = Config {
            seed: Some(42),
            dims: parse_dims("minecraft:overworld, minecraft:the_nether"),
            ..Config::default()
        };
        assert!(config.grants_seed());
        let dims = config.dims_for_policy();
        assert_eq!(dims.len(), 2);
        assert_eq!(dims[1].kind, "the_nether");
        for dim in dims {
            assert!(dim.has_seed);
            assert_eq!(dim.seed, 42);
        }
    }

    #[test]
    fn blank_dim_list_falls_back_to_the_overworld() {
        assert_eq!(parse_dims("  ,,  "), default_dims());
        assert_eq!(parse_dims("").len(), 1);
    }

    #[test]
    fn unknown_dimensions_are_not_marked_predictable() {
        let spec = spec_for("mymod:arena");
        assert_eq!(spec.kind, "arena");
        assert!(!spec.predictable);
    }
}
