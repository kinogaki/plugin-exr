//! Why an EXR could not be read or written.
use std::fmt;

/// Why an EXR could not be read or written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file breaks the format; `at` is the byte offset where it was
    /// found.
    Corrupt {
        /// What is wrong.
        what: &'static str,
        /// Byte offset into the file.
        at: usize,
    },
    /// A valid EXR this library does not read (deep data, mipmaps,
    /// subsampled channels, B44 or DWA compression).
    Unsupported(&'static str),
    /// The request itself is wrong (an image or part that cannot be
    /// written).
    Invalid(&'static str),
    /// The image is larger than the caller allows.
    TooLarge,
    /// The read's budget refused what the file asks: why.
    Budget(String),
    /// The read was cancelled.
    Cancelled,
    /// A file could not be read or written (the file helpers).
    Io(String),
}

impl Error {
    pub(crate) fn corrupt(what: &'static str, at: usize) -> Self {
        Error::Corrupt { what, at }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Corrupt { what, at } => write!(f, "invalid EXR at byte {at}: {what}"),
            Error::Unsupported(what) => write!(f, "unsupported EXR: {what}"),
            Error::Invalid(what) => write!(f, "invalid EXR request: {what}"),
            Error::TooLarge => write!(f, "EXR image exceeds the size limit"),
            Error::Budget(why) => write!(f, "EXR image: {why}"),
            Error::Cancelled => write!(f, "EXR read cancelled"),
            Error::Io(what) => write!(f, "EXR file: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// What the read's budget refused, or its cancellation.
impl From<kinogaki_studio_api::Error> for Error {
    fn from(e: kinogaki_studio_api::Error) -> Self {
        match e.is_cancelled() {
            true => Error::Cancelled,
            false => Error::Budget(e.message().to_owned()),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}
