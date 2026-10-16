//! Anvil MCA region file scanner and chunk NBT parser.
//!
//! Provides bounded, zero-panic parsing of Minecraft Anvil region (`.mca`) files.
//! Scans chunk locations, decompresses chunk payloads (Gzip or Zlib), and parses
//! the root NBT compound to determine chunk modification timestamp and generation status.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use flate2::read::{GzDecoder, ZlibDecoder};
use thiserror::Error;

/// Bytes per sector in Minecraft Anvil `.mca` files.
pub const SECTOR_BYTES: usize = 4096;

/// Number of chunk locations in the MCA header table (32x32).
pub const CHUNKS_PER_MCA_SIDE: i32 = 32;
pub const LOCATION_TABLE_ENTRIES: usize = (CHUNKS_PER_MCA_SIDE * CHUNKS_PER_MCA_SIDE) as usize;
pub const LOCATION_TABLE_BYTES: usize = LOCATION_TABLE_ENTRIES * 4;

/// Maximum compressed payload bytes to read for a single chunk (safety cap).
pub const MAX_INLINE_CHUNK_BYTES: usize = 255 * SECTOR_BYTES;

/// Maximum decompressed chunk NBT bytes before aborting (safety cap: 64MB).
pub const MAX_DECOMPRESSED_CHUNK_BYTES: u64 = 64 * 1024 * 1024;

/// Errors that can occur when reading or parsing Anvil MCA files.
#[derive(Debug, Error)]
pub enum AnvilError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("MCA file not found for region ({0}, {1})")]
    FileNotFound(i32, i32),

    #[error("MCA file at {path} is too small to contain header table (len={len})")]
    TruncatedHeader {
        path: PathBuf,
        len: u64,
    },

    #[error("Chunk at ({chunk_x}, {chunk_z}) has out-of-bounds sector offset {sector_offset} (file len={file_len})")]
    OutOfBoundsSector {
        chunk_x: i32,
        chunk_z: i32,
        sector_offset: u32,
        file_len: u64,
    },

    #[error("Chunk at ({chunk_x}, {chunk_z}) is not allocated or empty in MCA")]
    ChunkNotPresent {
        chunk_x: i32,
        chunk_z: i32,
    },

    #[error("Unsupported compression scheme {scheme} for chunk at ({chunk_x}, {chunk_z})")]
    UnsupportedCompression {
        chunk_x: i32,
        chunk_z: i32,
        scheme: u8,
    },

    #[error("Decompression failed for chunk at ({chunk_x}, {chunk_z}): {source}")]
    Decompress {
        chunk_x: i32,
        chunk_z: i32,
        #[source]
        source: std::io::Error,
    },

    #[error("Decompressed chunk NBT exceeds safety cap ({MAX_DECOMPRESSED_CHUNK_BYTES} bytes) at ({chunk_x}, {chunk_z})")]
    ChunkTooLarge {
        chunk_x: i32,
        chunk_z: i32,
    },

    #[error("NBT decoding error for chunk at ({chunk_x}, {chunk_z}): {source}")]
    Nbt {
        chunk_x: i32,
        chunk_z: i32,
        #[source]
        source: fastnbt::error::Error,
    },
}

/// Metadata and status for a single chunk inside an MCA region file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSummary {
    /// World chunk X coordinate.
    pub chunk_x: i32,
    /// World chunk Z coordinate.
    pub chunk_z: i32,
    /// Last modification timestamp (milliseconds or epoch seconds, from chunk NBT `LastUpdate` or file mtime).
    pub mtime: i64,
    /// Generation status string (e.g. "full", "minecraft:full", "features", etc.).
    pub status: String,
    /// Whether the chunk is considered fully generated ("full" or "minecraft:full").
    pub is_generated: bool,
}

/// Resolves the path to `r.{mca_x}.{mca_z}.mca` inside `<base_data_dir>/world/region` or `<base_data_dir>/region`.
pub fn find_mca_path(base_dir: &Path, mca_x: i32, mca_z: i32) -> Option<PathBuf> {
    let filename = format!("r.{mca_x}.{mca_z}.mca");
    let candidates = [
        base_dir.join("world").join("region").join(&filename),
        base_dir.join("region").join(&filename),
    ];
    for candidate in &candidates {
        if candidate.is_file() {
            return Some(candidate.clone());
        }
    }
    None
}

/// Calculates the location table index for a chunk inside an MCA file:
/// `(chunk_z % 32) * 32 + (chunk_x % 32)`.
///
/// In Minecraft coordinates, local MCA coordinates are `floor_mod(chunk, 32)` in [0, 31].
#[inline]
pub fn location_table_index(chunk_x: i32, chunk_z: i32) -> usize {
    let local_x = chunk_x.rem_euclid(CHUNKS_PER_MCA_SIDE) as usize;
    let local_z = chunk_z.rem_euclid(CHUNKS_PER_MCA_SIDE) as usize;
    local_z * (CHUNKS_PER_MCA_SIDE as usize) + local_x
}

/// Reads the chunk location table entry for a given chunk coordinate from a 4096-byte table.
///
/// Returns `Some((offset_sectors, sector_count))` or `None` if unallocated (entry == 0).
pub fn read_location_entry(location_table: &[u8; LOCATION_TABLE_BYTES], chunk_x: i32, chunk_z: i32) -> Option<(u32, u8)> {
    let idx = location_table_index(chunk_x, chunk_z) * 4;
    let raw = u32::from_be_bytes([
        location_table[idx],
        location_table[idx + 1],
        location_table[idx + 2],
        location_table[idx + 3],
    ]);
    if raw == 0 {
        return None;
    }
    let offset_sectors = raw >> 8;
    let sector_count = (raw & 0xFF) as u8;
    if offset_sectors < 2 || sector_count == 0 {
        return None;
    }
    Some((offset_sectors, sector_count))
}

/// Reads and parses an individual chunk from an already open MCA file.
///
/// - `offset_sectors`: 24-bit sector offset in the file
/// - `sector_count`: 8-bit sector count
/// - `file_mtime`: Fallback timestamp (ms) from file metadata if NBT lacks `LastUpdate`
pub fn read_chunk_from_file(
    file: &mut File,
    path: &Path,
    chunk_x: i32,
    chunk_z: i32,
    offset_sectors: u32,
    sector_count: u8,
    file_mtime: i64,
) -> Result<ChunkSummary, AnvilError> {
    let byte_offset = (offset_sectors as u64) * (SECTOR_BYTES as u64);
    let file_len = file.metadata().map_err(|e| AnvilError::Io {
        path: path.to_path_buf(),
        source: e,
    })?.len();

    if byte_offset + 5 > file_len {
        return Err(AnvilError::OutOfBoundsSector {
            chunk_x,
            chunk_z,
            sector_offset: offset_sectors,
            file_len,
        });
    }

    file.seek(SeekFrom::Start(byte_offset)).map_err(|e| AnvilError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    // 4-byte length + 1-byte compression scheme
    let mut header = [0u8; 5];
    file.read_exact(&mut header).map_err(|e| AnvilError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let payload_len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let scheme_byte = header[4];

    // payload_len includes the 1 byte for compression scheme
    if payload_len < 1 {
        return Err(AnvilError::ChunkNotPresent { chunk_x, chunk_z });
    }
    let compressed_len = payload_len - 1;

    // Verify bounds against allocated sector space and safety cap
    let max_allocated = (sector_count as usize) * SECTOR_BYTES;
    if payload_len > max_allocated || compressed_len > MAX_INLINE_CHUNK_BYTES {
        return Err(AnvilError::OutOfBoundsSector {
            chunk_x,
            chunk_z,
            sector_offset: offset_sectors,
            file_len,
        });
    }

    let mut compressed_buf = vec![0u8; compressed_len];
    file.read_exact(&mut compressed_buf).map_err(|e| AnvilError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Decompress based on compression scheme: 1 = Gzip, 2 = Zlib, 3 = uncompressed
    let decompressed = match scheme_byte & 0x7F {
        1 => {
            let mut decoder = GzDecoder::new(&compressed_buf[..]);
            let mut out = Vec::new();
            let mut limited = (&mut decoder).take(MAX_DECOMPRESSED_CHUNK_BYTES + 1);
            limited.read_to_end(&mut out).map_err(|e| AnvilError::Decompress {
                chunk_x,
                chunk_z,
                source: e,
            })?;
            if out.len() as u64 > MAX_DECOMPRESSED_CHUNK_BYTES {
                return Err(AnvilError::ChunkTooLarge { chunk_x, chunk_z });
            }
            out
        }
        2 => {
            let mut decoder = ZlibDecoder::new(&compressed_buf[..]);
            let mut out = Vec::new();
            let mut limited = (&mut decoder).take(MAX_DECOMPRESSED_CHUNK_BYTES + 1);
            limited.read_to_end(&mut out).map_err(|e| AnvilError::Decompress {
                chunk_x,
                chunk_z,
                source: e,
            })?;
            if out.len() as u64 > MAX_DECOMPRESSED_CHUNK_BYTES {
                return Err(AnvilError::ChunkTooLarge { chunk_x, chunk_z });
            }
            out
        }
        3 => compressed_buf,
        other => {
            return Err(AnvilError::UnsupportedCompression {
                chunk_x,
                chunk_z,
                scheme: other,
            });
        }
    };

    // Parse NBT
    let nbt_value: fastnbt::Value = fastnbt::from_bytes(&decompressed).map_err(|e| AnvilError::Nbt {
        chunk_x,
        chunk_z,
        source: e,
    })?;

    let (mtime, status, is_generated) = extract_chunk_status(&nbt_value, file_mtime);

    Ok(ChunkSummary {
        chunk_x,
        chunk_z,
        mtime,
        status,
        is_generated,
    })
}

/// Scans a single chunk by world coordinates from `<base_data_dir>`.
pub fn scan_chunk(base_dir: &Path, chunk_x: i32, chunk_z: i32) -> Result<ChunkSummary, AnvilError> {
    let mca_x = chunk_x.div_euclid(CHUNKS_PER_MCA_SIDE);
    let mca_z = chunk_z.div_euclid(CHUNKS_PER_MCA_SIDE);

    let mca_path = find_mca_path(base_dir, mca_x, mca_z).ok_or(AnvilError::FileNotFound(mca_x, mca_z))?;
    let mut file = File::open(&mca_path).map_err(|e| AnvilError::Io {
        path: mca_path.clone(),
        source: e,
    })?;

    let meta = file.metadata().map_err(|e| AnvilError::Io {
        path: mca_path.clone(),
        source: e,
    })?;
    let file_mtime = meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0i64, |d| d.as_millis() as i64);

    let mut table_buf = [0u8; LOCATION_TABLE_BYTES];
    file.read_exact(&mut table_buf).map_err(|e| AnvilError::Io {
        path: mca_path.clone(),
        source: e,
    })?;

    let Some((offset, sectors)) = read_location_entry(&table_buf, chunk_x, chunk_z) else {
        return Err(AnvilError::ChunkNotPresent { chunk_x, chunk_z });
    };

    read_chunk_from_file(&mut file, &mca_path, chunk_x, chunk_z, offset, sectors, file_mtime)
}

/// Scans all present chunks in an MCA file `r.{mca_x}.{mca_z}.mca`.
pub fn scan_mca_file(mca_path: &Path, mca_x: i32, mca_z: i32) -> Result<Vec<ChunkSummary>, AnvilError> {
    let mut file = File::open(mca_path).map_err(|e| AnvilError::Io {
        path: mca_path.to_path_buf(),
        source: e,
    })?;

    let meta = file.metadata().map_err(|e| AnvilError::Io {
        path: mca_path.to_path_buf(),
        source: e,
    })?;
    let file_len = meta.len();
    if file_len < LOCATION_TABLE_BYTES as u64 {
        return Err(AnvilError::TruncatedHeader {
            path: mca_path.to_path_buf(),
            len: file_len,
        });
    }

    let file_mtime = meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0i64, |d| d.as_millis() as i64);

    let mut table_buf = [0u8; LOCATION_TABLE_BYTES];
    file.read_exact(&mut table_buf).map_err(|e| AnvilError::Io {
        path: mca_path.to_path_buf(),
        source: e,
    })?;

    let mut chunks = Vec::new();
    let base_chunk_x = mca_x * CHUNKS_PER_MCA_SIDE;
    let base_chunk_z = mca_z * CHUNKS_PER_MCA_SIDE;

    for local_z in 0..CHUNKS_PER_MCA_SIDE {
        for local_x in 0..CHUNKS_PER_MCA_SIDE {
            let chunk_x = base_chunk_x + local_x;
            let chunk_z = base_chunk_z + local_z;
            if let Some((offset, sectors)) = read_location_entry(&table_buf, chunk_x, chunk_z) {
                match read_chunk_from_file(&mut file, mca_path, chunk_x, chunk_z, offset, sectors, file_mtime) {
                    Ok(summary) => chunks.push(summary),
                    Err(AnvilError::ChunkNotPresent { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }

    Ok(chunks)
}

fn extract_chunk_status(val: &fastnbt::Value, file_mtime: i64) -> (i64, String, bool) {
    // Check legacy "Level" compound or modern root
    let root = match val {
        fastnbt::Value::Compound(map) => {
            if let Some(level) = map.get("Level") {
                level
            } else {
                val
            }
        }
        _ => return (file_mtime, String::new(), false),
    };

    let status = get_string_from_compound(root, "Status")
        .or_else(|| get_string_from_compound(root, "status"))
        .unwrap_or_default();

    let is_generated = status == "full" || status == "minecraft:full";

    let mtime = get_long_from_compound(root, "LastUpdate")
        .or_else(|| get_long_from_compound(root, "lastUpdate"))
        .unwrap_or(file_mtime);

    (mtime, status.to_string(), is_generated)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use flate2::write::{GzEncoder, ZlibEncoder};
    use flate2::Compression;

    fn make_temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("confluxmap_anvil_test_{unique}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn location_table_index_calculation() {
        assert_eq!(location_table_index(0, 0), 0);
        assert_eq!(location_table_index(1, 0), 1);
        assert_eq!(location_table_index(0, 1), 32);
        assert_eq!(location_table_index(31, 31), 1023);
        assert_eq!(location_table_index(32, 32), 0);
        assert_eq!(location_table_index(-1, -1), 1023);
        assert_eq!(location_table_index(-32, -32), 0);
    }

    fn build_test_nbt(status: &str, last_update: i64) -> Vec<u8> {
        let mut map = std::collections::HashMap::new();
        map.insert("Status".to_string(), fastnbt::Value::String(status.to_string()));
        map.insert("LastUpdate".to_string(), fastnbt::Value::Long(last_update));
        let root = fastnbt::Value::Compound(map);
        fastnbt::to_bytes(&root).unwrap()
    }

    #[test]
    fn roundtrip_read_zlib_chunk() {
        let temp_dir = make_temp_dir();
        let region_dir = temp_dir.join("world").join("region");
        std::fs::create_dir_all(&region_dir).unwrap();
        let mca_path = region_dir.join("r.0.0.mca");

        let nbt_bytes = build_test_nbt("minecraft:full", 123456789);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&nbt_bytes).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut file_bytes = vec![0u8; SECTOR_BYTES * 3];
        // chunk (0, 0) -> location entry index 0
        // sector offset = 2, sector count = 1
        let entry = (2u32 << 8) | 1u32;
        file_bytes[0..4].copy_from_slice(&entry.to_be_bytes());

        // write chunk header at sector 2 (offset 8192)
        let chunk_start = SECTOR_BYTES * 2;
        let payload_len = (compressed.len() + 1) as u32;
        file_bytes[chunk_start..chunk_start + 4].copy_from_slice(&payload_len.to_be_bytes());
        file_bytes[chunk_start + 4] = 2; // zlib
        file_bytes[chunk_start + 5..chunk_start + 5 + compressed.len()].copy_from_slice(&compressed);

        std::fs::write(&mca_path, &file_bytes).unwrap();

        let summary = scan_chunk(&temp_dir, 0, 0).unwrap();
        assert_eq!(summary.chunk_x, 0);
        assert_eq!(summary.chunk_z, 0);
        assert_eq!(summary.status, "minecraft:full");
        assert!(summary.is_generated);
        assert_eq!(summary.mtime, 123456789);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn roundtrip_read_gzip_chunk() {
        let temp_dir = make_temp_dir();
        let region_dir = temp_dir.join("region");
        std::fs::create_dir_all(&region_dir).unwrap();
        let mca_path = region_dir.join("r.1.-1.mca");

        let nbt_bytes = build_test_nbt("features", 987654321);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&nbt_bytes).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut file_bytes = vec![0u8; SECTOR_BYTES * 3];
        let chunk_x = 32; // mca_x = 1, local_x = 0
        let chunk_z = -32; // mca_z = -1, local_z = 0
        let entry = (2u32 << 8) | 1u32;
        file_bytes[0..4].copy_from_slice(&entry.to_be_bytes());

        let chunk_start = SECTOR_BYTES * 2;
        let payload_len = (compressed.len() + 1) as u32;
        file_bytes[chunk_start..chunk_start + 4].copy_from_slice(&payload_len.to_be_bytes());
        file_bytes[chunk_start + 4] = 1; // gzip
        file_bytes[chunk_start + 5..chunk_start + 5 + compressed.len()].copy_from_slice(&compressed);

        std::fs::write(&mca_path, &file_bytes).unwrap();

        let summary = scan_chunk(&temp_dir, chunk_x, chunk_z).unwrap();
        assert_eq!(summary.chunk_x, chunk_x);
        assert_eq!(summary.chunk_z, chunk_z);
        assert_eq!(summary.status, "features");
        assert!(!summary.is_generated);
        assert_eq!(summary.mtime, 987654321);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn scan_mca_file_collects_all_allocated_chunks() {
        let temp_dir = make_temp_dir();
        let mca_path = temp_dir.join("r.0.0.mca");

        let nbt_1 = build_test_nbt("full", 100);
        let nbt_2 = build_test_nbt("minecraft:full", 200);

        let mut enc1 = ZlibEncoder::new(Vec::new(), Compression::default());
        enc1.write_all(&nbt_1).unwrap();
        let comp1 = enc1.finish().unwrap();

        let mut enc2 = ZlibEncoder::new(Vec::new(), Compression::default());
        enc2.write_all(&nbt_2).unwrap();
        let comp2 = enc2.finish().unwrap();

        let mut file_bytes = vec![0u8; SECTOR_BYTES * 4];
        // chunk (0, 0) at sector 2
        let entry1 = (2u32 << 8) | 1u32;
        file_bytes[0..4].copy_from_slice(&entry1.to_be_bytes());
        let chunk1_start = SECTOR_BYTES * 2;
        file_bytes[chunk1_start..chunk1_start + 4].copy_from_slice(&((comp1.len() + 1) as u32).to_be_bytes());
        file_bytes[chunk1_start + 4] = 2;
        file_bytes[chunk1_start + 5..chunk1_start + 5 + comp1.len()].copy_from_slice(&comp1);

        // chunk (31, 31) at sector 3
        let idx2 = location_table_index(31, 31) * 4;
        let entry2 = (3u32 << 8) | 1u32;
        file_bytes[idx2..idx2 + 4].copy_from_slice(&entry2.to_be_bytes());
        let chunk2_start = SECTOR_BYTES * 3;
        file_bytes[chunk2_start..chunk2_start + 4].copy_from_slice(&((comp2.len() + 1) as u32).to_be_bytes());
        file_bytes[chunk2_start + 4] = 2;
        file_bytes[chunk2_start + 5..chunk2_start + 5 + comp2.len()].copy_from_slice(&comp2);

        std::fs::write(&mca_path, &file_bytes).unwrap();

        let all = scan_mca_file(&mca_path, 0, 0).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].chunk_x, 0);
        assert_eq!(all[0].chunk_z, 0);
        assert_eq!(all[0].mtime, 100);
        assert_eq!(all[1].chunk_x, 31);
        assert_eq!(all[1].chunk_z, 31);
        assert_eq!(all[1].mtime, 200);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
