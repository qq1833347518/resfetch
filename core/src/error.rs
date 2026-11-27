use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    EmptyInput,
    UnsupportedUrl(String),
    InvalidUrl(String),
    InvalidCookie(String),
    InvalidPlaylist(String),
    InvalidEpub(String),
    Io(String),
    Platform(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::EmptyInput => write!(f, "输入为空"),
            Error::UnsupportedUrl(u) => write!(f, "不支持的链接：{}", u),
            Error::InvalidUrl(u) => write!(f, "链接格式非法：{}", u),
            Error::InvalidCookie(m) => write!(f, "Cookie 格式错误：{}", m),
            Error::InvalidPlaylist(m) => write!(f, "m3u8 格式错误：{}", m),
            Error::InvalidEpub(m) => write!(f, "EPUB 构建失败：{}", m),
            Error::Io(m) => write!(f, "IO 错误：{}", m),
            Error::Platform(m) => write!(f, "平台能力不可用：{}", m),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
