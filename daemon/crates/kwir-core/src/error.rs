use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid configuration at {path}: {message}")]
    InvalidConfig { path: PathBuf, message: String },
    #[error("vault ID already exists: {0}")]
    DuplicateVaultId(String),
    #[error("vault path must be an absolute, readable directory: {0}")]
    InvalidVaultPath(PathBuf),
    #[error("no vaults are registered")]
    NoVaults,
    #[error("index error: {0}")]
    Index(String),
    #[error("query error: {0}")]
    Query(String),
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("data-root lock is already held at {0}")]
    LockHeld(PathBuf),
    #[error("state error: {0}")]
    State(String),
    #[error("semantic error: {0}")]
    Semantic(String),
    #[error("semantic search is unavailable: {0}")]
    SemanticUnavailable(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
