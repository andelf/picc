use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechError {
    Unsupported(&'static str),
    InvalidConfig(String),
    Engine(String),
    Writeback(String),
}

impl fmt::Display for SpeechError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(msg) => write!(f, "{msg}"),
            Self::InvalidConfig(msg) => write!(f, "{msg}"),
            Self::Engine(msg) => write!(f, "{msg}"),
            Self::Writeback(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for SpeechError {}
