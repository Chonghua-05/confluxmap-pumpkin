//! World save inspection and cascade resolution for confluxmap-pumpkin.
//!
//! Provides robust, standalone extraction of world information (seed, version name,
//! data version) from a Minecraft / Pumpkin world archive using a cascade reader
//! strategy with full Gzip + NBT decompression. Zero unwrap, strict error handling.

#![allow(clippy::collapsible_if, clippy::type_complexity, clippy::question_mark)]

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use flate2::read::GzDecoder;
use thiserror::Error;

/// Errors that can occur while inspecting a world directory or archive.
#[derive(Debug, Error)]
pub enum WorldError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Gzip decompression error at {path}: {source}")]
    Decompress {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("NBT decoding error at {path}: {source}")]
    Nbt {
        path: PathBuf,
        #[source]
        source: fastnbt::error::Error,
    },

    #[error("No valid world information found in candidate paths for base: {0}")]
    NotFound(PathBuf),
}

/// Identifies the archive source and file path from which world information was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldInfoSource {
    /// Primary source: `<base>/world/data/minecraft/world_gen_settings.dat` (or `<base>/world_gen_settings.dat`)
    WorldGenSettings(PathBuf),
    /// Secondary source: `<base>/world/dimensions/minecraft/overworld/data/minecraft/world_gen_settings.dat`
    DimensionWorldGenSettings(PathBuf),
    /// Tertiary source: `<base>/world/level.dat` (or `<base>/level.dat`)
    LevelDat(PathBuf),
    /// Quaternary source: `<base>/world/level.dat_old` (or `<base>/level.dat_old`)
    LevelDatOld(PathBuf),
}

/// Information extracted from a Minecraft world save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldInfo {
    /// World seed used for terrain and feature generation.
    pub seed: i64,
    /// Friendly Minecraft version string (e.g. "1.21.4" or "26.2"), if present.
    pub version_name: Option<String>,
    /// Minecraft DataVersion integer (e.g. 4189), if present.
    pub data_version: Option<i32>,
    /// Which candidate file provided the seed.
    pub source: WorldInfoSource,
}

/// Inspects a base directory using the cascade resolution order and returns the resolved [`WorldInfo`].
///
/// Order of evaluation:
/// 1. Primary: `<base>/world/data/minecraft/world_gen_settings.dat` or `<base>/world_gen_settings.dat`
/// 2. Secondary: `<base>/world/dimensions/minecraft/overworld/data/minecraft/world_gen_settings.dat`
/// 3. Tertiary: `<base>/world/level.dat` or `<base>/level.dat`
/// 4. Quaternary: `<base>/world/level.dat_old` or `<base>/level.dat_old`
///
/// If the seed is found in a `world_gen_settings.dat` file, version information is also
/// opportunistically extracted from `level.dat` (or `level.dat_old`) if available.
pub fn inspect_world(base_dir: &Path) -> Result<WorldInfo, WorldError> {
    // 1. Primary: <base>/world/data/minecraft/world_gen_settings.dat (or <base>/world_gen_settings.dat)
    let primary_candidates = [
        base_dir.join("world").join("data").join("minecraft").join("world_gen_settings.dat"),
        base_dir.join("world_gen_settings.dat"),
    ];
    for path in &primary_candidates {
        if path.is_file() {
            if let Some(seed) = read_seed_from_world_gen_settings(path)? {
                let (version_name, data_version) = try_extract_version(base_dir);
                return Ok(WorldInfo {
                    seed,
                    version_name,
                    data_version,
                    source: WorldInfoSource::WorldGenSettings(path.clone()),
                });
            }
        }
    }

    // 2. Secondary: <base>/world/dimensions/minecraft/overworld/data/minecraft/world_gen_settings.dat
    let secondary_path = base_dir
        .join("world")
        .join("dimensions")
        .join("minecraft")
        .join("overworld")
        .join("data")
        .join("minecraft")
        .join("world_gen_settings.dat");
    if secondary_path.is_file() {
        if let Some(seed) = read_seed_from_world_gen_settings(&secondary_path)? {
            let (version_name, data_version) = try_extract_version(base_dir);
            return Ok(WorldInfo {
                seed,
                version_name,
                data_version,
                source: WorldInfoSource::DimensionWorldGenSettings(secondary_path),
            });
        }
    }

    // 3. Tertiary: <base>/world/level.dat (or <base>/level.dat)
    let tertiary_candidates = [
        base_dir.join("world").join("level.dat"),
        base_dir.join("level.dat"),
    ];
    for path in &tertiary_candidates {
        if path.is_file() {
            if let Some((seed, v_name, d_ver)) = read_level_dat(path)? {
                return Ok(WorldInfo {
                    seed,
                    version_name: v_name,
                    data_version: d_ver,
                    source: WorldInfoSource::LevelDat(path.clone()),
                });
            }
        }
    }

    // 4. Quaternary: <base>/world/level.dat_old (or <base>/level.dat_old)
    let quaternary_candidates = [
        base_dir.join("world").join("level.dat_old"),
        base_dir.join("level.dat_old"),
    ];
    for path in &quaternary_candidates {
        if path.is_file() {
            if let Some((seed, v_name, d_ver)) = read_level_dat(path)? {
                return Ok(WorldInfo {
                    seed,
                    version_name: v_name,
                    data_version: d_ver,
                    source: WorldInfoSource::LevelDatOld(path.clone()),
                });
            }
        }
    }

    Err(WorldError::NotFound(base_dir.to_path_buf()))
}

/// Reads a gzip-compressed NBT file into a `fastnbt::Value`.
fn read_gzip_nbt(path: &Path) -> Result<fastnbt::Value, WorldError> {
    let file = File::open(path).map_err(|source| WorldError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut decoder = GzDecoder::new(file);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).map_err(|source| WorldError::Decompress {
        path: path.to_path_buf(),
        source,
    })?;

    fastnbt::from_bytes(&decompressed).map_err(|source| WorldError::Nbt {
        path: path.to_path_buf(),
        source,
    })
}

/// Extracts a seed from a `world_gen_settings.dat` file.
///
/// Looks for `"seed"` at top level, or nested under `"data"` or `"Data"` compounds.
fn read_seed_from_world_gen_settings(path: &Path) -> Result<Option<i64>, WorldError> {
    let nbt = read_gzip_nbt(path)?;
    Ok(extract_seed_from_settings(&nbt))
}

fn extract_seed_from_settings(mut val: &fastnbt::Value) -> Option<i64> {
    loop {
        if let Some(seed) = get_long_from_compound(val, "seed") {
            return Some(seed);
        }
        if let Some(inner) = get_compound(val, "data").or_else(|| get_compound(val, "Data")) {
            val = inner;
        } else {
            return None;
        }
    }
}

/// Reads seed and version information from a `level.dat` or `level.dat_old` file.
fn read_level_dat(path: &Path) -> Result<Option<(i64, Option<String>, Option<i32>)>, WorldError> {
    let nbt = read_gzip_nbt(path)?;
    let Some(data) = get_compound(&nbt, "Data").or_else(|| get_compound(&nbt, "data")) else {
        return Ok(None);
    };

    let seed = extract_seed_from_level_data(data);
    let (version_name, data_version) = extract_version_from_level_data(data);

    match seed {
        Some(s) => Ok(Some((s, version_name, data_version))),
        None => Ok(None),
    }
}

/// Tries to read version information from `level.dat` or `level.dat_old` under `<base>`.
fn try_extract_version(base_dir: &Path) -> (Option<String>, Option<i32>) {
    let candidates = [
        base_dir.join("world").join("level.dat"),
        base_dir.join("level.dat"),
        base_dir.join("world").join("level.dat_old"),
        base_dir.join("level.dat_old"),
    ];

    for path in &candidates {
        if path.is_file() {
            if let Ok(nbt) = read_gzip_nbt(path) {
                if let Some(data) = get_compound(&nbt, "Data").or_else(|| get_compound(&nbt, "data")) {
                    let (v_name, d_ver) = extract_version_from_level_data(data);
                    if v_name.is_some() || d_ver.is_some() {
                        return (v_name, d_ver);
                    }
                }
            }
        }
    }

    (None, None)
}

fn extract_seed_from_level_data(data: &fastnbt::Value) -> Option<i64> {
    // 1. Data.WorldGenSettings.seed
    if let Some(wgs) = get_compound(data, "WorldGenSettings").or_else(|| get_compound(data, "world_gen_settings")) {
        if let Some(seed) = get_long_from_compound(wgs, "seed") {
            return Some(seed);
        }
    }

    // 2. Data.RandomSeed
    if let Some(seed) = get_long_from_compound(data, "RandomSeed").or_else(|| get_long_from_compound(data, "random_seed")) {
        return Some(seed);
    }

    None
}

fn extract_version_from_level_data(data: &fastnbt::Value) -> (Option<String>, Option<i32>) {
    let mut version_name = None;
    let mut data_version = None;

    if let Some(ver_comp) = get_compound(data, "Version").or_else(|| get_compound(data, "version")) {
        if let Some(name) = get_string_from_compound(ver_comp, "Name").or_else(|| get_string_from_compound(ver_comp, "name")) {
            version_name = Some(name.to_string());
        }
        if let Some(id) = get_int_from_compound(ver_comp, "Id").or_else(|| get_int_from_compound(ver_comp, "id")) {
            data_version = Some(id);
        }
    }

    if data_version.is_none() {
        if let Some(dv) = get_int_from_compound(data, "DataVersion").or_else(|| get_int_from_compound(data, "data_version")) {
            data_version = Some(dv);
        }
    }

    (version_name, data_version)
}

// --- Helper accessors on `fastnbt::Value` ---

fn get_compound<'a>(val: &'a fastnbt::Value, key: &str) -> Option<&'a fastnbt::Value> {
    match val {
        fastnbt::Value::Compound(map) => {
            let v = map.get(key)?;
            match v {
                fastnbt::Value::Compound(_) => Some(v),
                _ => None,
            }
        }
        _ => None,
    }
}

fn get_long_from_compound(val: &fastnbt::Value, key: &str) -> Option<i64> {
    match val {
        fastnbt::Value::Compound(map) => match map.get(key)? {
            fastnbt::Value::Long(v) => Some(*v),
            fastnbt::Value::Int(v) => Some(*v as i64),
            fastnbt::Value::Short(v) => Some(*v as i64),
            fastnbt::Value::Byte(v) => Some(*v as i64),
            _ => None,
        },
        _ => None,
    }
}

fn get_int_from_compound(val: &fastnbt::Value, key: &str) -> Option<i32> {
    match val {
        fastnbt::Value::Compound(map) => match map.get(key)? {
            fastnbt::Value::Int(v) => Some(*v),
            fastnbt::Value::Long(v) => i32::try_from(*v).ok(),
            fastnbt::Value::Short(v) => Some(*v as i32),
            _ => None,
        },
        _ => None,
    }
}

fn get_string_from_compound<'a>(val: &'a fastnbt::Value, key: &str) -> Option<&'a str> {
    match val {
        fastnbt::Value::Compound(map) => match map.get(key)? {
            fastnbt::Value::String(s) => Some(s.as_str()),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::collections::HashMap;
    use std::fs;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("confluxmap_world_test_{unique}_{}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_gzip_nbt(path: &Path, value: &fastnbt::Value) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        let bytes = fastnbt::to_bytes(value).expect("serialize nbt");
        let file = File::create(path).expect("create file");
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(&bytes).expect("write gzip");
        encoder.finish().expect("finish gzip");
    }

    #[test]
    fn cascade_primary_world_gen_settings() {
        let temp = make_temp_dir();
        let settings_path = temp
            .join("world")
            .join("data")
            .join("minecraft")
            .join("world_gen_settings.dat");

        // Structure: { "data": { "seed": 42_i64 } }
        let mut inner = HashMap::new();
        inner.insert("seed".to_string(), fastnbt::Value::Long(42));
        let mut root = HashMap::new();
        root.insert("data".to_string(), fastnbt::Value::Compound(inner));

        write_gzip_nbt(&settings_path, &fastnbt::Value::Compound(root));

        let info = inspect_world(&temp).expect("inspect success");
        assert_eq!(info.seed, 42);
        assert_eq!(info.version_name, None);
        assert_eq!(info.data_version, None);
        assert_eq!(info.source, WorldInfoSource::WorldGenSettings(settings_path));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn cascade_primary_fallback_root_path_with_opportunistic_version() {
        let temp = make_temp_dir();
        let settings_path = temp.join("world_gen_settings.dat");

        // Direct seed at root compound: { "seed": -999_i64 }
        let mut root = HashMap::new();
        root.insert("seed".to_string(), fastnbt::Value::Long(-999));
        write_gzip_nbt(&settings_path, &fastnbt::Value::Compound(root));

        // Add a level.dat with version info
        let level_path = temp.join("level.dat");
        let mut ver = HashMap::new();
        ver.insert("Name".to_string(), fastnbt::Value::String("26.2".to_string()));
        ver.insert("Id".to_string(), fastnbt::Value::Int(4903));
        let mut data = HashMap::new();
        data.insert("Version".to_string(), fastnbt::Value::Compound(ver));
        let mut level_root = HashMap::new();
        level_root.insert("Data".to_string(), fastnbt::Value::Compound(data));
        write_gzip_nbt(&level_path, &fastnbt::Value::Compound(level_root));

        let info = inspect_world(&temp).expect("inspect success");
        assert_eq!(info.seed, -999);
        assert_eq!(info.version_name.as_deref(), Some("26.2"));
        assert_eq!(info.data_version, Some(4903));
        assert_eq!(info.source, WorldInfoSource::WorldGenSettings(settings_path));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn cascade_secondary_dimension_world_gen_settings() {
        let temp = make_temp_dir();
        let sec_path = temp
            .join("world")
            .join("dimensions")
            .join("minecraft")
            .join("overworld")
            .join("data")
            .join("minecraft")
            .join("world_gen_settings.dat");

        // Structure: { "Data": { "data": { "seed": 123456789_i64 } } }
        let mut inner = HashMap::new();
        inner.insert("seed".to_string(), fastnbt::Value::Long(123456789));
        let mut mid = HashMap::new();
        mid.insert("data".to_string(), fastnbt::Value::Compound(inner));
        let mut root = HashMap::new();
        root.insert("Data".to_string(), fastnbt::Value::Compound(mid));

        write_gzip_nbt(&sec_path, &fastnbt::Value::Compound(root));

        let info = inspect_world(&temp).expect("inspect success");
        assert_eq!(info.seed, 123456789);
        assert_eq!(info.source, WorldInfoSource::DimensionWorldGenSettings(sec_path));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn cascade_tertiary_level_dat_world_gen_settings() {
        let temp = make_temp_dir();
        let level_path = temp.join("world").join("level.dat");

        // Structure: Data.WorldGenSettings.seed, Data.Version.Name, Data.DataVersion
        let mut wgs = HashMap::new();
        wgs.insert("seed".to_string(), fastnbt::Value::Long(55555));

        let mut ver = HashMap::new();
        ver.insert("Name".to_string(), fastnbt::Value::String("1.21.4".to_string()));

        let mut data = HashMap::new();
        data.insert("WorldGenSettings".to_string(), fastnbt::Value::Compound(wgs));
        data.insert("Version".to_string(), fastnbt::Value::Compound(ver));
        data.insert("DataVersion".to_string(), fastnbt::Value::Int(4189));

        let mut root = HashMap::new();
        root.insert("Data".to_string(), fastnbt::Value::Compound(data));

        write_gzip_nbt(&level_path, &fastnbt::Value::Compound(root));

        let info = inspect_world(&temp).expect("inspect success");
        assert_eq!(info.seed, 55555);
        assert_eq!(info.version_name.as_deref(), Some("1.21.4"));
        assert_eq!(info.data_version, Some(4189));
        assert_eq!(info.source, WorldInfoSource::LevelDat(level_path));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn cascade_tertiary_level_dat_random_seed_fallback() {
        let temp = make_temp_dir();
        let level_path = temp.join("level.dat");

        // Structure: Data.RandomSeed
        let mut data = HashMap::new();
        data.insert("RandomSeed".to_string(), fastnbt::Value::Long(88888));

        let mut root = HashMap::new();
        root.insert("Data".to_string(), fastnbt::Value::Compound(data));

        write_gzip_nbt(&level_path, &fastnbt::Value::Compound(root));

        let info = inspect_world(&temp).expect("inspect success");
        assert_eq!(info.seed, 88888);
        assert_eq!(info.version_name, None);
        assert_eq!(info.data_version, None);
        assert_eq!(info.source, WorldInfoSource::LevelDat(level_path));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn cascade_quaternary_level_dat_old() {
        let temp = make_temp_dir();
        let old_path = temp.join("world").join("level.dat_old");

        let mut wgs = HashMap::new();
        wgs.insert("seed".to_string(), fastnbt::Value::Long(101010));

        let mut data = HashMap::new();
        data.insert("WorldGenSettings".to_string(), fastnbt::Value::Compound(wgs));

        let mut root = HashMap::new();
        root.insert("Data".to_string(), fastnbt::Value::Compound(data));

        write_gzip_nbt(&old_path, &fastnbt::Value::Compound(root));

        let info = inspect_world(&temp).expect("inspect success");
        assert_eq!(info.seed, 101010);
        assert_eq!(info.source, WorldInfoSource::LevelDatOld(old_path));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn cascade_not_found_when_empty() {
        let temp = make_temp_dir();
        let err = inspect_world(&temp).expect_err("should fail when no world archive exists");
        assert!(matches!(err, WorldError::NotFound(_)));
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn handles_corrupted_gzip() {
        let temp = make_temp_dir();
        let corrupted_path = temp.join("world_gen_settings.dat");
        fs::write(&corrupted_path, b"not a valid gzip stream").expect("write bad file");

        let err = inspect_world(&temp).expect_err("should fail on corrupt gzip");
        assert!(matches!(err, WorldError::Decompress { .. }));
        let _ = fs::remove_dir_all(&temp);
    }
}
