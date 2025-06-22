use std::fmt;

#[derive(Debug)]
pub enum TurbineError {
    ConfigError(String),
    ContainerError(String),
    NetworkError(String),
    FilesystemError(String),
    ProcessError(String),
    SecurityError(String),
    RuntimeError(String),
    IoError(std::io::Error),
    SerdeError(toml::de::Error),
    SerdeSerError(toml::ser::Error),
}

impl fmt::Display for TurbineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TurbineError::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            TurbineError::ContainerError(msg) => write!(f, "Container error: {}", msg),
            TurbineError::NetworkError(msg) => write!(f, "Network error: {}", msg),
            TurbineError::FilesystemError(msg) => write!(f, "Filesystem error: {}", msg),
            TurbineError::ProcessError(msg) => write!(f, "Process error: {}", msg),
            TurbineError::SecurityError(msg) => write!(f, "Security error: {}", msg),
            TurbineError::RuntimeError(msg) => write!(f, "Runtime error: {}", msg),
            TurbineError::IoError(err) => write!(f, "IO error: {}", err),
            TurbineError::SerdeError(err) => write!(f, "TOML deserialization error: {}", err),
            TurbineError::SerdeSerError(err) => write!(f, "TOML serialization error: {}", err),
        }
    }
}

impl std::error::Error for TurbineError {}

impl From<std::io::Error> for TurbineError {
    fn from(err: std::io::Error) -> Self {
        TurbineError::IoError(err)
    }
}

impl From<toml::de::Error> for TurbineError {
    fn from(err: toml::de::Error) -> Self {
        TurbineError::SerdeError(err)
    }
}

impl From<toml::ser::Error> for TurbineError {
    fn from(err: toml::ser::Error) -> Self {
        TurbineError::SerdeSerError(err)
    }
}

impl From<anyhow::Error> for TurbineError {
    fn from(err: anyhow::Error) -> Self {
        TurbineError::RuntimeError(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, TurbineError>;
