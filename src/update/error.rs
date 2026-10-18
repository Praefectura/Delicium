//! Error type for the updater: manifest handling, fetching, hashing and
//! installation.
//!
//! Every failure is reported instead of panicking: this code consumes data
//! from the network and from a directory a previous run may have left in any
//! state, so unexpected bytes and unexpected on-disk layouts are normal
//! inputs, not programming errors.

use std::path::PathBuf;

use thiserror::Error;

/// What the updater cannot do, and why.
#[derive(Debug, Error)]
pub enum UpdateError {
    /// The manifest is not valid JSON, has an unknown field, or violates a
    /// declared limit.
    #[error("update manifest is invalid: {0}")]
    Manifest(String),

    /// A version string, tag or channel is not acceptable.
    #[error("update version is invalid: {0}")]
    Version(String),

    /// The manifest describes a release this build must not install.
    #[error("update is refused: {0}")]
    Refused(String),

    /// A transfer failed below the HTTP layer (DNS, connect, TLS, timeout).
    #[error("network error: {0}")]
    Network(String),

    /// The server answered with a status other than 200.
    #[error("server returned HTTP {0}")]
    Http(u32),

    /// A payload exceeded its declared size or a global limit.
    #[error("{what} is {actual} bytes, which exceeds the {limit} byte limit")]
    TooLarge {
        what: String,
        actual: u64,
        limit: u64,
    },

    /// A payload ended before its declared size.
    #[error("{path} ended after {actual} of {expected} bytes")]
    Truncated {
        path: String,
        actual: u64,
        expected: u64,
    },

    /// A payload did not hash to its declared digest.
    #[error("{path} has SHA-256 {actual}, expected {expected}")]
    HashMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    /// The on-disk state or install directory could not be used.
    #[error("update state is invalid: {0}")]
    State(String),

    /// A filesystem operation failed.
    #[error("cannot {action} `{path}`: {detail}")]
    Io {
        action: &'static str,
        path: PathBuf,
        detail: String,
    },
}

impl UpdateError {
    /// Convenience constructor for [`UpdateError::Manifest`].
    pub fn manifest(detail: impl Into<String>) -> Self {
        Self::Manifest(detail.into())
    }

    /// Convenience constructor for [`UpdateError::Refused`].
    pub fn refused(detail: impl Into<String>) -> Self {
        Self::Refused(detail.into())
    }

    /// Convenience constructor for [`UpdateError::Network`].
    pub fn network(detail: impl Into<String>) -> Self {
        Self::Network(detail.into())
    }

    /// Convenience constructor for [`UpdateError::State`].
    pub fn state(detail: impl Into<String>) -> Self {
        Self::State(detail.into())
    }

    /// Convenience constructor for [`UpdateError::Io`].
    pub fn io(action: &'static str, path: impl Into<PathBuf>, error: std::io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            detail: error.to_string(),
        }
    }
}
