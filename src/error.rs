use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    Msg { message: String },

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("command `{program}` failed ({code}): {detail}")]
    Command {
        program: String,
        code: i32,
        detail: String,
    },

    #[error("`{program}` was not found on PATH")]
    MissingTool { program: String },
}

impl Error {
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Msg {
            message: message.into(),
        }
    }
}

pub fn io_at(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
