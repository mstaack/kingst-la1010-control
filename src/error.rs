use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// Bad arguments or options (exit 1).
    Usage(String),
    /// Device missing, USB failure, overrun (exit 2).
    Device(String),
    /// Firmware missing or unparsable (exit 2).
    Firmware(String),
    /// Trigger, --until or --count not reached (exit 3).
    NoResult(String),
    /// File or stream I/O (exit 2).
    Io(std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 1,
            Error::NoResult(_) => 3,
            Error::Device(_) | Error::Firmware(_) | Error::Io(_) => 2,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usage(m) | Error::Device(m) | Error::Firmware(m) | Error::NoResult(m) => {
                f.write_str(m)
            }
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
