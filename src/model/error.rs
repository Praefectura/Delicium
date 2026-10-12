//! Error type for model loading, validation, compilation and evaluation.

use std::path::PathBuf;

use thiserror::Error;

/// Every failure mode of the native model pipeline.
///
/// Loading never panics: malformed, oversized, non-finite or structurally
/// inconsistent input is reported through this type.
#[derive(Debug, Error)]
pub enum ModelError {
    /// The byte payload exceeds [`LoadLimits::max_file_bytes`](crate::model::LoadLimits::max_file_bytes).
    #[error("model payload is {actual} bytes, which exceeds the {max} byte limit")]
    FileTooLarge { actual: u64, max: usize },

    /// Reading the file itself failed.
    #[error("failed to read model file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The payload is not valid JSON, or does not match the schema
    /// (including unknown fields).
    #[error("model JSON is invalid: {message}")]
    InvalidJson { message: String },

    /// `format` was not the exact expected string.
    #[error("unsupported model format `{found}` (expected `delicium.model`)")]
    UnsupportedFormat { found: String },

    /// `format_version` was not the exact expected string.
    #[error("unsupported model format_version `{found}` (expected `0.1`)")]
    UnsupportedVersion { found: String },

    /// The model name is empty.
    #[error("model name must not be empty")]
    EmptyName,

    /// The payload for the declared `kind` is missing.
    #[error("model of kind `{kind}` is missing its `{kind}` payload")]
    MissingPayload { kind: &'static str },

    /// A payload for a kind that does not match the declared `kind` is present.
    #[error("model of kind `{kind}` must not carry a `{other}` payload")]
    UnexpectedPayload {
        kind: &'static str,
        other: &'static str,
    },

    /// A value is structurally wrong (empty name, non-finite number,
    /// out of range, wrong length, ...). `context` names the exact location.
    #[error("{context}: {detail}")]
    Invalid { context: String, detail: String },

    /// A count exceeds a configured prototype budget.
    #[error("{what} budget exceeded: {actual} > {max}")]
    LimitExceeded {
        what: &'static str,
        actual: u64,
        max: u64,
    },

    /// An arithmetic operation on untrusted sizes overflowed; the model is
    /// rejected instead of wrapping or allocating.
    #[error("size accounting overflowed while computing {what}")]
    SizeOverflow { what: &'static str },

    /// A name reference could not be resolved.
    #[error("unresolved {kind} reference `{name}` in {context}")]
    UnknownReference {
        kind: &'static str,
        name: String,
        context: String,
    },

    /// The same name was declared twice in one namespace.
    #[error("duplicate {kind} name `{name}`")]
    DuplicateName { kind: &'static str, name: String },

    /// A parent/child relationship (deformer or joint hierarchy) contains a cycle.
    #[error("{context} contains a parent cycle")]
    Cycle { context: String },

    /// A runtime index passed to an evaluation function is out of range.
    #[error("{context}: index {index} is out of range (len {len})")]
    OutOfRange {
        context: String,
        index: usize,
        len: usize,
    },

    /// Input handed to a public evaluation function is inconsistent with the
    /// compiled model (wrong slice length, non-finite value, ...).
    #[error("invalid evaluation input: {detail}")]
    InvalidInput { detail: String },

    /// The joint hierarchy could not be inverted for skinning (singular bind pose).
    #[error("joint `{joint}` has a singular bind transform and cannot be bound")]
    SingularBindPose { joint: String },
}

impl ModelError {
    /// Convenience constructor for [`ModelError::Invalid`].
    pub(crate) fn invalid(context: impl Into<String>, detail: impl Into<String>) -> Self {
        ModelError::Invalid {
            context: context.into(),
            detail: detail.into(),
        }
    }
}
