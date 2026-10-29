use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decode: {0}")]
    Decode(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("resample: {0}")]
    Resample(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("peak cache: {0}")]
    Peaks(String),
}

impl MediaError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        MediaError::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<symphonia::core::errors::Error> for MediaError {
    fn from(e: symphonia::core::errors::Error) -> Self {
        MediaError::Decode(e.to_string())
    }
}

impl From<hound::Error> for MediaError {
    fn from(e: hound::Error) -> Self {
        MediaError::Encode(e.to_string())
    }
}
