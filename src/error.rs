use crate::PartialChatResponse;
use std::{fmt, time::Duration};

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidRequest,
    UnsupportedOption,
    AuthError,
    RateLimitError,
    ProviderError,
    TransportError,
    StreamInterrupted,
    Configuration,
    Routing,
}

/// Error text intentionally never includes raw provider bodies, URLs or credentials.
#[derive(Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub request_id: Option<String>,
    pub retry_after: Option<Duration>,
    pub retryable: bool,
    pub partial: Option<Box<PartialChatResponse>>,
    message: String,
}
impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            request_id: None,
            retry_after: None,
            retryable: false,
            partial: None,
            message: message.into(),
        }
    }
    pub(crate) fn protocol() -> Self {
        Self::new(
            ErrorKind::ProviderError,
            "invalid or unsupported provider response",
        )
    }
    pub(crate) fn unsupported(option: &str) -> Self {
        Self::new(
            ErrorKind::UnsupportedOption,
            format!("unsupported option: {option}"),
        )
    }
    pub(crate) fn transport(retryable: bool) -> Self {
        Self {
            retryable,
            ..Self::new(
                ErrorKind::TransportError,
                "transport failed or deadline exceeded",
            )
        }
    }
    pub(crate) fn interrupted(partial: PartialChatResponse) -> Self {
        Self {
            partial: Some(Box::new(partial)),
            ..Self::new(
                ErrorKind::StreamInterrupted,
                "stream ended without successful completion",
            )
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("has_partial", &self.partial.is_some())
            .finish_non_exhaustive()
    }
}
impl std::error::Error for Error {}
