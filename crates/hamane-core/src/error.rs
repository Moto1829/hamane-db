use thiserror::Error;

/// hamane-db 全体のエラー型。
#[derive(Debug, Error)]
pub enum HamaneError {
    #[error("dimension mismatch: collection expects {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("collection already exists: {0}")]
    CollectionExists(String),

    #[error("collection not found: {0}")]
    CollectionNotFound(String),

    #[error("invalid config: {0}")]
    InvalidConfig(String),

    #[error("invalid vector: {0}")]
    InvalidVector(String),

    #[error("corrupted data: {0}")]
    Corrupted(String),

    #[error("database is locked by another process: {0}")]
    Locked(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
