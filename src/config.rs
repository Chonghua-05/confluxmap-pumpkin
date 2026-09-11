//! Runtime configuration: the plugin's own file, in its own data folder.
//!
//! Pumpkin gives a plugin no way to read the world seed. The plugin API has no
//! seed accessor, and the WASI sandbox preopens exactly one directory - the
//! plugin's private data folder - so neither `pumpkin.toml` nor the world save
//! is reachable. The seed has to be written down instead, and this module owns
//! that file:
//!
//! ```text
//! plugins/data/confluxmap-pumpkin/config.toml
//! ```
//!
//! The first load writes an annotated template when the file is absent; the
//! operator then uncomments `seed` and fills it in. `/cfm reload` reads the file
//! again, so an edit needs no restart.
//!
//! Parsing is lenient on purpose, and dependency-free: one `key = value` per
//! line, `#` comments, values bare or quoted, lists as either a TOML array or a
//! comma-separated string. A bad value warns and leaves that one setting at its
//! default - the plugin must still answer the handshake (with `seedGranted = 0`)
//! rather than refuse to load over a typo.

use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;

use crate::protocol::{Budgets, Dim};

/// The plugin's name. Pumpkin derives the data folder from the plugin metadata,
/// so the paths below are built from this one constant.
pub const PLUGIN_NAME: &str = "confluxmap-pumpkin";

/// The configuration file, relative to the plugin's data folder.
pub const CONFIG_FILE: &str = "config.toml";

/// The annotated file written when no configuration exists yet.
///
/// Every key is commented out, so a freshly written file means "all defaults" -
/// which is also what [`parse`] must make of it. The test suite asserts that.
pub const TEMPLATE: &str = "\
# confluxmap-pumpkin configuration.
#
# Pumpkin offers a plugin no way to read the world seed: the plugin API has no
# seed accessor, and this folder is the only directory the plugin can open. The
# seed therefore has to be written here, and it must match `seed` in pumpkin.toml
# - clients predict terrain from it, so a wrong value renders a wrong map.
#
# Run `/cfm reload` after editing, or restart the server.

# The world seed. Uncomment and fill in. While the line stays commented out the
# plugin answers with seedGranted=0 and clients render no map. Accepts a signed
# decimal integer, or an unsigned one for a seed Minecraft displays above
# 9223372036854775807.
# seed = 0

# Set to false to answer handshakes without granting the seed. Clients then
# render no map. Default: true.
# share_seed = true

# The worldgen version sent to clients, e.g. \"1.21.4\". Leave unset to use the
# Minecraft version of the running server.
# worldgen = \"\"

# The client-side cache namespace. Leave unset to derive it from the seed.
# world_id = \"\"

# Dimensions to advertise, comma separated, e.g.
# \"minecraft:overworld,minecraft:the_nether\". Default: minecraft:overworld.
# dims = \"minecraft:overworld\"

# Whether the shared-waypoint channel is served. Set to false to answer that
# channel's handshake with enabled=false and refuse every mutation. Default: true.
# share_waypoints = true

# Whether a non-operator may create, edit, or delete their own waypoints. Set to
# false to reserve management for operators while everyone still views.
# Default: true.
# allow_non_operator_waypoint_management = true

# Most waypoints kept per world, over all players. Values outside 1-512 are
# clamped. Default: 512.
# max_waypoints_per_world = 512

# Most waypoints one player may publish, further capped by the per-world limit.
# Values outside 1-64 are clamped. Default: 64.
# max_waypoints_per_player = 64

# Waypoint changes one player may make per minute; excess is throttled. Values
# outside 1-6000 are clamped. Default: 30.
# waypoint_mutations_per_minute = 30
";

/// Minecraft version baked into the `pumpkin-plugin-api` build this plugin was
/// compiled against. Used only when the running server's version string cannot
/// be parsed.
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
    /// The world seed, or `None` when the file has no parseable `seed`.
    pub seed: Option<i64>,
    /// Whether the seed may be granted to clients at all (`share_seed`).
    pub share_seed: bool,
    /// Explicit `worldgen`, when set.
    pub worldgen_override: Option<String>,
    /// Explicit `world_id`, when set.
    pub world_id_override: Option<String>,
    /// Dimensions to advertise.
    pub dims: Vec<DimSpec>,
    /// Advertised rate/batch limits.
    pub budgets: Budgets,
    /// Whether the shared-waypoint channel is served at all (`share_waypoints`).
    pub share_waypoints: bool,
    /// Whether a non-operator may manage the waypoints they published.
    pub allow_non_operator_waypoint_management: bool,
    /// Ceiling on waypoints retained per world.
    pub max_waypoints_per_world: u32,
    /// Ceiling on waypoints a single player may publish.
    pub max_waypoints_per_player: u32,
    /// Waypoint mutations a single player may make per minute.
    pub waypoint_mutations_per_minute: u32,
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
            share_waypoints: true,
            allow_non_operator_waypoint_management: true,
            max_waypoints_per_world: 512,
            max_waypoints_per_player: 64,
            waypoint_mutations_per_minute: 30,
        }
    }
}

/// The configuration read from disk, plus anything worth telling the operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// The resolved configuration. Always usable: failures leave defaults.
    pub config: Config,
    /// Problems to log: a file that could not be created or read, a value that
    /// did not parse, a key that is not recognised.
    pub warnings: Vec<String>,
}

/// Reads the configuration from the plugin's data folder.
///
/// `data_folder` is the sandbox-visible path the host reports (currently
/// `data`). A missing file is not an error: the template is written in its
/// place, and the caller is told about it through [`Loaded::warnings`].
pub fn load(data_folder: &str) -> Loaded {
    let path = format!("{data_folder}/{CONFIG_FILE}");
    let mut warnings = Vec::new();

    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            match fs::write(&path, TEMPLATE) {
                Ok(()) => warnings.push(format!(
                    "wrote a new configuration to {} - uncomment `seed` and fill it in",
                    operator_path()
                )),
                Err(error) => warnings.push(format!(
                    "could not write {}: {error}; running on defaults",
                    operator_path()
                )),
            }
            TEMPLATE.to_string()
        }
        Err(error) => {
            warnings.push(format!(
                "could not read {}: {error}; running on defaults",
                operator_path()
            ));
            String::new()
        }
    };

    let config = parse(&source, &mut warnings);
    Loaded { config, warnings }
}

/// Where the operator finds this file, relative to the server root.
///
/// For log and command text only. Inside the sandbox the same file is reached as
/// `<data folder>/config.toml`.
pub fn operator_path() -> String {
    format!("plugins/data/{PLUGIN_NAME}/{CONFIG_FILE}")
}

impl Config {
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
    /// 1. `worldgen`, for setups where the server version string is unusable.
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
        let _ = writeln!(out, "share_waypoints = {}", self.share_waypoints);
        let _ = writeln!(
            out,
            "allow_non_operator_waypoint_management = {}",
            self.allow_non_operator_waypoint_management
        );
        let _ = writeln!(
            out,
            "max_waypoints_per_world = {}",
            self.max_waypoints_per_world
        );
        let _ = writeln!(
            out,
            "max_waypoints_per_player = {}",
            self.max_waypoints_per_player
        );
        let _ = writeln!(
            out,
            "waypoint_mutations_per_minute = {}",
            self.waypoint_mutations_per_minute
        );
        out
    }

    fn worldgen_source(&self, server_version: Option<&str>) -> &'static str {
        if self.worldgen_override.is_some() {
            "worldgen override"
        } else if server_version.and_then(parse_mc_version).is_some() {
            "derived from server version"
        } else {
            "compile-time fallback"
        }
    }

    /// Clamps the waypoint limits into the ranges the server actually honours.
    ///
    /// The Paper companion applies the same bounds in `ServerConfig.normalize()`
    /// and does so silently, so an out-of-range value is a configuration to
    /// correct, not something worth warning about on every reload.
    fn normalize(&mut self) {
        self.max_waypoints_per_world = self.max_waypoints_per_world.clamp(1, 512);
        // The per-player ceiling can never exceed what the world retains.
        self.max_waypoints_per_player = self
            .max_waypoints_per_player
            .clamp(1, self.max_waypoints_per_world);
        self.waypoint_mutations_per_minute = self.waypoint_mutations_per_minute.clamp(1, 6000);
    }
}

/// Parses the file body, appending anything wrong with it to `warnings`.
fn parse(source: &str, warnings: &mut Vec<String>) -> Config {
    let mut config = Config::default();
    let mut unknown: Vec<String> = Vec::new();

    for (index, raw) in source.lines().enumerate() {
        let line = strip_comment(raw).trim();
        // Blank lines and table headers are not this plugin's business: the file
        // is flat, but an operator pasting a sectioned fragment should not get a
        // warning for it.
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let number = index + 1;

        let Some((raw_key, raw_value)) = line.split_once('=') else {
            warnings.push(format!("line {number}: not a `key = value` pair: {line}"));
            continue;
        };
        let key = unquote(raw_key.trim());
        let value = unquote(raw_value.trim());

        match key.as_str() {
            "seed" => match parse_seed(&value) {
                Some(seed) => config.seed = Some(seed),
                None => warnings.push(format!(
                    "line {number}: `seed = {value}` is not an integer; leaving the seed unset"
                )),
            },
            "share_seed" => match parse_bool(&value) {
                Some(share) => config.share_seed = share,
                None => warnings.push(format!(
                    "line {number}: `share_seed = {value}` is not a boolean; keeping {}",
                    config.share_seed
                )),
            },
            "worldgen" => config.worldgen_override = non_empty(value),
            "world_id" => config.world_id_override = non_empty(value),
            "dims" => config.dims = parse_dims(&value),
            "share_waypoints" => match parse_bool(&value) {
                Some(enabled) => config.share_waypoints = enabled,
                None => warnings.push(format!(
                    "line {number}: `share_waypoints = {value}` is not a boolean; keeping {}",
                    config.share_waypoints
                )),
            },
            "allow_non_operator_waypoint_management" => match parse_bool(&value) {
                Some(allowed) => config.allow_non_operator_waypoint_management = allowed,
                None => warnings.push(format!(
                    "line {number}: `allow_non_operator_waypoint_management = {value}` is not a boolean; keeping {}",
                    config.allow_non_operator_waypoint_management
                )),
            },
            "max_waypoints_per_world" => match parse_u32(&value) {
                Some(limit) => config.max_waypoints_per_world = limit,
                None => warnings.push(format!(
                    "line {number}: `max_waypoints_per_world = {value}` is not a positive integer; keeping {}",
                    config.max_waypoints_per_world
                )),
            },
            "max_waypoints_per_player" => match parse_u32(&value) {
                Some(limit) => config.max_waypoints_per_player = limit,
                None => warnings.push(format!(
                    "line {number}: `max_waypoints_per_player = {value}` is not a positive integer; keeping {}",
                    config.max_waypoints_per_player
                )),
            },
            "waypoint_mutations_per_minute" => match parse_u32(&value) {
                Some(rate) => config.waypoint_mutations_per_minute = rate,
                None => warnings.push(format!(
                    "line {number}: `waypoint_mutations_per_minute = {value}` is not a positive integer; keeping {}",
                    config.waypoint_mutations_per_minute
                )),
            },
            _ => unknown.push(key),
        }
    }

    if !unknown.is_empty() {
        warnings.push(format!(
            "ignoring unrecognised {}: {}",
            if unknown.len() == 1 { "key" } else { "keys" },
            unknown.join(", ")
        ));
    }
    config.normalize();
    config
}

/// Cuts a `#` comment, but not one inside a quoted value.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' | '\'' if quote == Some(ch) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            '#' if quote.is_none() => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Removes one layer of matching quotes, if present.
fn unquote(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        return raw[1..raw.len() - 1].to_string();
    }
    raw.to_string()
}

/// Splits a list value: either a TOML array (`["a", "b"]`) or a plain
/// comma-separated string. Blank entries are dropped.
fn list_values(raw: &str) -> Vec<String> {
    let body = raw
        .trim()
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(raw.trim());
    body.split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

/// Parses `dims`. An empty result falls back to the default, so the policy is
/// never dim-less.
fn parse_dims(raw: &str) -> Vec<DimSpec> {
    let specs: Vec<DimSpec> = list_values(raw).iter().map(|id| spec_for(id)).collect();
    if specs.is_empty() {
        default_dims()
    } else {
        specs
    }
}

/// Default dimension set: the overworld alone.
fn default_dims() -> Vec<DimSpec> {
    vec![spec_for("minecraft:overworld")]
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

/// Parses a seed, accepting both signed decimal and a raw `u64` bit pattern (so
/// a seed above `i64::MAX` can be written the way Minecraft displays it).
/// Underscores are accepted as digit separators.
fn parse_seed(raw: &str) -> Option<i64> {
    let trimmed = raw.trim().replace('_', "");
    if let Ok(v) = trimmed.parse::<i64>() {
        return Some(v);
    }
    trimmed.parse::<u64>().ok().map(|v| v as i64)
}

/// Parses a boolean flag: `1/true/yes/on` and `0/false/no/off`.
fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Parses a positive integer setting. Zero is rejected as well as non-integers:
/// an explicit `0` reads as "disabled/unset", and the caller's warning is more
/// useful than silently substituting the minimum.
fn parse_u32(raw: &str) -> Option<u32> {
    match raw.trim().parse::<u32>() {
        Ok(0) | Err(_) => None,
        Ok(value) => Some(value),
    }
}

/// `Some` unless the value is blank, which is how an empty override is written.
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_quiet(source: &str) -> Config {
        let mut warnings = Vec::new();
        parse(source, &mut warnings)
    }

    fn warnings_for(source: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        parse(source, &mut warnings);
        warnings
    }

    /// The file the plugin writes must mean "all defaults" when re-read, or a
    /// fresh install would start from something other than the documented state.
    #[test]
    fn the_template_parses_to_the_defaults() {
        let config = parse_quiet(TEMPLATE);
        assert_eq!(config, Config::default());
        assert_eq!(config.seed, None);
        assert!(!config.grants_seed());
    }

    #[test]
    fn parses_a_filled_in_file() {
        let config = parse_quiet(
            r#"
            # a comment
            seed = 12345
            share_seed = false
            worldgen = "1.21.4"
            world_id = "my-namespace"
            dims = "minecraft:overworld,minecraft:the_nether"
            "#,
        );
        assert_eq!(config.seed, Some(12345));
        assert!(!config.share_seed);
        assert_eq!(config.worldgen_override.as_deref(), Some("1.21.4"));
        assert_eq!(config.world_id_override.as_deref(), Some("my-namespace"));
        assert_eq!(config.dims.len(), 2);
        assert_eq!(config.dims[1].kind, "the_nether");
    }

    #[test]
    fn accepts_toml_arrays_for_dims() {
        let config = parse_quiet(r#"dims = ["minecraft:overworld", "minecraft:the_end"]"#);
        assert_eq!(config.dims.len(), 2);
        assert_eq!(config.dims[1].id, "minecraft:the_end");
        assert!(config.dims[1].predictable);
    }

    #[test]
    fn ignores_comments_blank_lines_and_table_headers() {
        let config = parse_quiet(
            "\n# seed = 1\n[plugins]\nseed = 7  # trailing comment\nworld_id = \"a#b\"\n",
        );
        assert_eq!(config.seed, Some(7));
        // A `#` inside quotes is part of the value, not a comment.
        assert_eq!(config.world_id_override.as_deref(), Some("a#b"));
    }

    #[test]
    fn a_bad_value_warns_and_keeps_the_default() {
        let warnings = warnings_for("seed = soon\nshare_seed = maybe\n");
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("`seed = soon`"), "{warnings:?}");
        assert!(warnings[1].contains("`share_seed = maybe`"), "{warnings:?}");
        let config = parse_quiet("seed = soon\nshare_seed = maybe\n");
        assert_eq!(config.seed, None);
        assert!(config.share_seed);
    }

    #[test]
    fn unknown_keys_are_reported_once() {
        let warnings = warnings_for("seed = 1\nwdogen = \"26.2\"\ndimz = \"x\"\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("wdogen, dimz"), "{warnings:?}");
    }

    #[test]
    fn a_line_without_a_value_is_reported() {
        let warnings = warnings_for("seed\n");
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("not a `key = value` pair"),
            "{warnings:?}"
        );
    }

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
        assert_eq!(
            parse_seed("1_789_288_297_874_145_099"),
            Some(1_789_288_297_874_145_099)
        );
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

    #[test]
    fn parses_the_waypoint_settings() {
        let config = parse_quiet(
            r#"
            share_waypoints = false
            allow_non_operator_waypoint_management = false
            max_waypoints_per_world = 256
            max_waypoints_per_player = 32
            waypoint_mutations_per_minute = 120
            "#,
        );
        assert!(!config.share_waypoints);
        assert!(!config.allow_non_operator_waypoint_management);
        assert_eq!(config.max_waypoints_per_world, 256);
        assert_eq!(config.max_waypoints_per_player, 32);
        assert_eq!(config.waypoint_mutations_per_minute, 120);
    }

    #[test]
    fn a_bad_waypoint_value_warns_and_keeps_the_default() {
        let source = "share_waypoints = maybe\nmax_waypoints_per_world = -1\n\
                      max_waypoints_per_player = 0\nwaypoint_mutations_per_minute = many\n";
        let warnings = warnings_for(source);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        let config = parse_quiet(source);
        // A zero limit is a typo, not a request to clamp to the minimum.
        assert!(config.share_waypoints);
        assert_eq!(config.max_waypoints_per_world, 512);
        assert_eq!(config.max_waypoints_per_player, 64);
        assert_eq!(config.waypoint_mutations_per_minute, 30);
    }

    #[test]
    fn waypoint_limits_are_clamped_into_range() {
        // The world cap clamps first, and the per-player cap then follows it.
        let config = parse_quiet(
            "max_waypoints_per_world = 2000\nmax_waypoints_per_player = 9999\n\
             waypoint_mutations_per_minute = 99999\n",
        );
        assert_eq!(config.max_waypoints_per_world, 512);
        assert_eq!(config.max_waypoints_per_player, 512);
        assert_eq!(config.waypoint_mutations_per_minute, 6000);
        // Clamping is silent: it is not a parse failure.
        assert!(warnings_for("max_waypoints_per_world = 2000\n").is_empty());
    }

    #[test]
    fn the_template_documents_every_waypoint_setting() {
        for key in [
            "share_waypoints",
            "allow_non_operator_waypoint_management",
            "max_waypoints_per_world",
            "max_waypoints_per_player",
            "waypoint_mutations_per_minute",
        ] {
            assert!(
                TEMPLATE.contains(&format!("# {key} =")),
                "template is missing a commented `{key}` key"
            );
        }
    }
}
