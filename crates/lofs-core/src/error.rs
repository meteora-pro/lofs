//! Unified error type for LOFS core operations.
//!
//! The MVP exposes the errors surfaced by the four L0 MCP tools
//! (`lofs.{create,list,mount,unmount}`). `MountAdvisory` and `PushConflict`
//! (rich cooperative-coordination payloads per ADR-002) land in Phase 1.2
//! together with the FUSE backend.

use thiserror::Error;

/// Errors produced by core LOFS operations.
#[derive(Debug, Error)]
pub enum LofsError {
    /// Bucket with the given identifier was not found in the registry.
    #[error("bucket not found: {0}")]
    NotFound(String),

    /// Another bucket already owns that name inside the org.
    #[error("bucket name already taken: {0}")]
    NameTaken(String),

    /// Supplied bucket name does not satisfy the naming rules.
    #[error("invalid bucket name `{name}`: {reason}")]
    InvalidName {
        /// The offending name as supplied.
        name: String,
        /// Human-readable reason why the name is rejected.
        reason: String,
    },

    /// TTL outside of the allowed range.
    #[error("invalid ttl_days={ttl_days}: {reason}")]
    InvalidTtl {
        /// The requested TTL, as supplied by the caller.
        ttl_days: i64,
        /// Reason the TTL is rejected.
        reason: String,
    },

    /// Size limit outside of the allowed range.
    #[error("invalid size_limit_mb={size_limit_mb}: {reason}")]
    InvalidSizeLimit {
        /// The requested size limit, as supplied by the caller.
        size_limit_mb: i64,
        /// Reason the limit is rejected.
        reason: String,
    },

    /// Requested operation is not implemented on the current platform.
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),

    /// OCI registry layer failure (HTTP / protocol / auth).
    #[error("registry error: {0}")]
    Registry(String),

    /// HTTP-level transport failure (reqwest).
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Catch-all for early prototype paths we have not typed yet.
    #[error("{0}")]
    Other(String),
}

/// Convenience alias for fallible core operations.
pub type LofsResult<T> = Result<T, LofsError>;

impl From<oci_client::errors::OciDistributionError> for LofsError {
    fn from(e: oci_client::errors::OciDistributionError) -> Self {
        Self::Registry(e.to_string())
    }
}
