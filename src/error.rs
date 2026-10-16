//! Central error definitions for confluxmap-pumpkin.

use std::path::PathBuf;
use thiserror::Error;

/// Result type alias using `PluginError`.
pub type Result<T> = std::result::Result<T, PluginError>;

/// Master error type covering all operational domains.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("NBT deserialization error: {0}")]
    Nbt(String),

    #[error("Gzip decompression error: {0}")]
    Decompress(#[from] std::io::Error),

    #[error("World archive not found in base data path: {0}")]
    WorldNotFound(PathBuf),

    #[error("Seed is missing from archive and no override configured")]
    MissingSeed,

    #[error("Corrupt level.dat: {0}")]
    CorruptLevelDat(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Config parsing error: {0}")]
    Config(String),

    #[error("Storage error: {0}")]
    Storage(String),
}

impl PluginError {
    /// Helper to wrap an I/O error with its contextual path.
    pub fn io(path: impl Into<PathBuf>, err: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source: err,
        }
    }
}
