//! `GhcrDriver` — GitHub Container Registry placeholder.
//!
//! **Status:** scaffold. Today this driver is a thin relabel of
//! [`GenericDriver`] with GHCR-appropriate rate-limit hints. It will gain
//! real overrides as we verify behaviour against `ghcr.io`:
//!
//! - GHCR hosts images under `ghcr.io/<owner>/<repo>` — almost always
//!   [`RepoMode::Shared`] maps naturally (owner/repo as the project
//!   prefix), which is what the default `effective_repo_mode` does.
//! - Authenticated GHCR allows `DELETE /v2/.../manifests/<digest>`, but
//!   the UI hides it — we expect `supports_native_delete=true` to hold.
//! - Rate limits: ~5000 authenticated req/hour per user, much tighter on
//!   anonymous paths. We cap concurrency accordingly.
//!
//! When a user hits a corner case we don't cover here, fall back to
//! `--driver generic` on the CLI as a workaround.

use std::time::Duration;

use async_trait::async_trait;

use super::{RateLimitPolicy, RegistryDriver};

/// Placeholder driver for GitHub Container Registry. Inherits every
/// default from [`RegistryDriver`] except the concurrency cap.
#[derive(Debug, Default, Clone, Copy)]
pub struct GhcrDriver;

impl GhcrDriver {
    /// Build a fresh driver.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RegistryDriver for GhcrDriver {
    fn name(&self) -> &'static str {
        "ghcr"
    }

    fn description(&self) -> &'static str {
        "GitHub Container Registry (scaffold — GHCR-specific overrides land as we validate)"
    }

    fn rate_limit_policy(&self) -> RateLimitPolicy {
        // GHCR enforces ~5k authenticated req/h per user and stricter
        // caps on anonymous access. A modest concurrency ceiling keeps
        // bursty `lofs list` from blowing the budget if a user runs it
        // in a loop.
        RateLimitPolicy {
            max_concurrent: Some(8),
            retry_after_header: "Retry-After",
            default_backoff: Duration::from_secs(5),
            max_retries: 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghcr_identifies_itself() {
        let d = GhcrDriver::new();
        assert_eq!(d.name(), "ghcr");
        assert!(d.description().contains("GitHub"));
        assert_eq!(d.rate_limit_policy().max_concurrent, Some(8));
    }

    #[test]
    fn ghcr_inherits_sensible_defaults() {
        let d = GhcrDriver::new();
        assert!(d.supports_artifact_type());
        assert!(d.supports_native_delete());
        assert!(d.catalog_supported());
    }
}
