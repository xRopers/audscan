use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },

    #[error("invalid manifest: {0}")]
    ManifestParse(#[from] serde_json::Error),

    #[error("manifest version {found} is not supported (this build reads version {expected})")]
    ManifestVersion { found: u32, expected: u32 },

    #[error("input does not match the manifest: {0}")]
    SourceMismatch(String),

    #[error("audio {id} at offset {offset:#x}: {reason}")]
    Audio { id: u32, offset: u64, reason: String },

    #[error("manifest names an unsafe output file {0:?} (must be a plain file name)")]
    BadFilename(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn io_err(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io { path: path.to_path_buf(), source }
}
