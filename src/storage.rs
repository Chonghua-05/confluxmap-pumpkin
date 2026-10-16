//! Robust, atomic storage utilities for persisted files in WASI data directory.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{PluginError, Result};

/// Atomically writes bytes to `target_path` via a temporary sibling file and rename.
///
/// This prevents half-written or corrupted files if the process terminates mid-write.
pub fn atomic_write_bytes(target_path: &Path, content: &[u8]) -> Result<()> {
    let parent = target_path.parent().unwrap_or_else(|| Path::new("."));

    let file_name = target_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let tmp_path: PathBuf = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));

    let mut tmp_file = File::create(&tmp_path).map_err(|e| PluginError::io(&tmp_path, e))?;
    tmp_file
        .write_all(content)
        .map_err(|e| PluginError::io(&tmp_path, e))?;
    tmp_file
        .flush()
        .map_err(|e| PluginError::io(&tmp_path, e))?;
    tmp_file
        .sync_all()
        .map_err(|e| PluginError::io(&tmp_path, e))?;
    drop(tmp_file);

    fs::rename(&tmp_path, target_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        PluginError::io(target_path, e)
    })?;

    Ok(())
}

/// Atomically writes string content.
pub fn atomic_write_str(target_path: &Path, content: &str) -> Result<()> {
    atomic_write_bytes(target_path, content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_atomic_write_roundtrip() {
        let dir = std::env::temp_dir().join("confluxmap_test_storage");
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("test.json");
        let sample = "{\"test\": 123}";

        atomic_write_str(&target, sample).expect("write succeeded");
        let read_back = fs::read_to_string(&target).expect("read succeeded");
        assert_eq!(read_back, sample);

        let _ = fs::remove_dir_all(&dir);
    }
}
