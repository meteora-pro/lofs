//! Registry-flavour abstraction — `RegistryDriver`.
//!
//! OCI Distribution is a spec, but every real-world registry reads it
//! slightly differently. Zot honours the full OCI 1.1 surface with zero
//! config; CNCF Distribution tops out at OCI 1.1 too but defaults DELETE
//! off; GitLab Container Registry runs a tight allow-list on media types
//! and closes the public manifest-DELETE endpoint; GHCR caps HTTP
//! throughput to ~5k requests/hour; Harbor wraps projects + robot
//! accounts on top of Distribution.
//!
//! A single `OciRegistry` client needs to handle all of them without
//! branching on hostname strings at a hundred call sites. This module
//! gives that behaviour a type-level home:
//!
//! - [`RegistryDriver`] — trait every flavour implements. Default methods
//!   encode "standard OCI 1.1"; specific drivers override only the bits
//!   that differ (DELETE fallback, media-type tolerance, rate limit).
//! - [`detect_from_url`] — picks a driver based on hostname; the CLI can
//!   override with `--driver` / `LOFS_DRIVER`.
//! - [`DeleteContext`] — value object passed to `delete_manifest` so the
//!   trait doesn't need to carry the whole `OciRegistry` state.
//! - [`RateLimitPolicy`] — per-driver concurrency + backoff settings.
//!
//! The concrete drivers live in sibling modules: `generic`, `gitlab`,
//! `ghcr`, `harbor`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use oci_client::Reference;
use oci_client::client::Client;
use oci_client::secrets::RegistryAuth;

use super::registry::RepoMode;
use crate::error::LofsResult;

pub mod generic;
pub mod gitlab;

pub use generic::GenericDriver;
pub use gitlab::GitLabDriver;

/// Result of auto-detection given a parsed base URL.
pub type DriverRef = Arc<dyn RegistryDriver + Send + Sync>;

/// Everything a driver needs to perform (or refuse) a DELETE against the
/// registry. Passed as a value object so `RegistryDriver` stays small.
pub struct DeleteContext<'a> {
    /// Plain HTTP client for raw requests the registry exposes.
    pub http: &'a reqwest::Client,
    /// `oci-client` client — used for token-exchange flows (Basic → JWT).
    pub oci: &'a Client,
    /// `scheme://host` only (no path).
    pub origin: &'a str,
    /// Full OCI Reference for the target manifest (repo path + tag).
    pub reference: &'a Reference,
    /// Resolved manifest digest (`sha256:...`).
    pub digest: &'a str,
    /// Active auth (we re-use it for any token-exchange round-trip).
    pub auth: &'a RegistryAuth,
    /// Path prefix from the base URL (may be empty). Useful for drivers
    /// that need to call a registry-specific native API on top of the
    /// bare OCI surface (GitLab: `/api/v4/projects/:prefix/...`).
    pub path_prefix: &'a str,
}

/// Per-driver HTTP concurrency + backoff settings.
#[derive(Clone, Debug)]
pub struct RateLimitPolicy {
    /// Max in-flight requests (applied via a semaphore at the registry layer).
    /// `None` means no artificial cap — default for local Zot / Distribution.
    pub max_concurrent: Option<usize>,
    /// Header name carrying the server's requested backoff on a 429.
    /// Defaults to `Retry-After`; some registries use custom names.
    pub retry_after_header: &'static str,
    /// Fallback backoff applied when the 429 carries no `Retry-After`.
    pub default_backoff: Duration,
    /// How many times to retry a single request on 429. Retries are
    /// sequential (exponential backoff is the retry policy's job, not
    /// the driver's — this is just a hard ceiling).
    pub max_retries: u32,
}

impl Default for RateLimitPolicy {
    fn default() -> Self {
        Self {
            max_concurrent: None,
            retry_after_header: "Retry-After",
            default_backoff: Duration::from_secs(1),
            max_retries: 3,
        }
    }
}

/// Behaviour surface that varies across OCI-registry implementations.
///
/// Every method has a sensible default matching "modern OCI 1.1" (Zot,
/// Distribution 3.x, Harbor with delete enabled). Specific drivers
/// override only the knobs that actually differ for them.
///
/// The trait is `#[async_trait]`-boxed so we can hold a driver as
/// `Arc<dyn RegistryDriver + Send + Sync>` — stable Rust's native
/// `async fn in traits` isn't object-safe yet.
#[async_trait]
pub trait RegistryDriver: std::fmt::Debug + Send + Sync + 'static {
    /// Short stable name used in CLI output and logs (`generic`, `gitlab`,
    /// `ghcr`, `harbor`, …).
    fn name(&self) -> &'static str;

    /// Human-readable hint shown by `lofs doctor`. Defaults to the driver
    /// `name()`; override to surface key behaviour differences.
    fn description(&self) -> &'static str {
        self.name()
    }

    /// Repo-addressing mode this driver prefers given the parsed URL.
    ///
    /// Default matches the historical auto-detection: empty `path_prefix`
    /// → [`RepoMode::Separate`] (one repo per bucket), non-empty →
    /// [`RepoMode::Shared`] (single repo, tag-encoded identity).
    fn effective_repo_mode(&self, path_prefix: &str) -> RepoMode {
        if path_prefix.is_empty() {
            RepoMode::Separate
        } else {
            RepoMode::Shared
        }
    }

    /// Whether this registry accepts the OCI 1.1 `artifactType` field on
    /// image manifests. GitLab CR is currently strict here; most others
    /// are permissive.
    fn supports_artifact_type(&self) -> bool {
        true
    }

    /// Whether the registry exposes a DELETE endpoint for manifests as
    /// part of the OCI Distribution API. When `false`, [`delete_manifest`]
    /// must perform the deletion via a registry-specific native API.
    fn supports_native_delete(&self) -> bool {
        true
    }

    /// Whether `/v2/_catalog` returns useful results. `Shared`-mode
    /// drivers never call it, but `Separate`-mode drivers rely on it.
    fn catalog_supported(&self) -> bool {
        true
    }

    /// Rate-limit settings applied to every outbound HTTP call.
    fn rate_limit_policy(&self) -> RateLimitPolicy {
        RateLimitPolicy::default()
    }

    /// Delete a manifest. Default implementation speaks bare OCI
    /// Distribution; drivers that need a native API (GitLab) override.
    ///
    /// Returns the raw HTTP response so the caller can decode 200/202/204
    /// as success and 404 as NotFound, etc.
    async fn delete_manifest<'a>(
        &self,
        ctx: &'a DeleteContext<'a>,
    ) -> LofsResult<reqwest::Response> {
        generic::perform_native_delete(ctx).await
    }
}

/// Auto-detect a driver based on the host part of the base URL (the
/// `path_prefix` is informational only — you can host GitLab at a custom
/// domain, so the hostname match stays a hint not a rule).
///
/// Returns a driver if the hostname matches a known pattern, else
/// [`GenericDriver`] (the default).
pub fn detect_from_url(host: &str) -> DriverRef {
    if is_gitlab_host(host) {
        return Arc::new(GitLabDriver::new());
    }
    // TODO(sprint): ghcr.io, *.harbor.* once dedicated drivers land.
    Arc::new(GenericDriver::new())
}

/// Pick a driver from a user-supplied name (`auto`, `generic`, `gitlab`,
/// …) or from the host if `auto`. Used by the CLI `--driver` flag.
pub fn driver_by_name_or_auto(name: &str, host: &str) -> LofsResult<DriverRef> {
    match name {
        "auto" => Ok(detect_from_url(host)),
        "generic" => Ok(Arc::new(GenericDriver::new())),
        "gitlab" => Ok(Arc::new(GitLabDriver::new())),
        other => Err(crate::error::LofsError::Other(format!(
            "unknown driver `{other}`; valid: auto | generic | gitlab"
        ))),
    }
}

fn is_gitlab_host(host: &str) -> bool {
    // Match the hosted GitLab.com registry as well as any registry the
    // self-hosted GitLab instance publishes — those commonly live under
    // `registry.<domain>` for a GitLab at `gitlab.<domain>`, so we match
    // on the registry prefix too.
    matches!(host, "registry.gitlab.com")
        || host.starts_with("registry.gitlab.")
        || host.contains(".gitlab.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_repo_mode_depends_on_prefix() {
        let g = GenericDriver::new();
        assert_eq!(g.effective_repo_mode(""), RepoMode::Separate);
        assert_eq!(g.effective_repo_mode("user/project"), RepoMode::Shared);
    }

    #[test]
    fn default_driver_is_permissive() {
        let g = GenericDriver::new();
        assert!(g.supports_artifact_type());
        assert!(g.supports_native_delete());
        assert!(g.catalog_supported());
        assert!(g.rate_limit_policy().max_concurrent.is_none());
    }

    #[test]
    fn detect_picks_gitlab_for_known_hosts() {
        assert_eq!(detect_from_url("registry.gitlab.com").name(), "gitlab");
        assert_eq!(
            detect_from_url("registry.gitlab.example.org").name(),
            "gitlab"
        );
        assert_eq!(
            detect_from_url("gitlab.acme.co").name(),
            "generic",
            "plain gitlab.foo.bar hostname is not the registry — default to generic"
        );
        assert_eq!(
            detect_from_url("code.gitlab.internal").name(),
            "gitlab",
            "self-hosted GitLab registries often live at code.gitlab.*"
        );
    }

    #[test]
    fn detect_falls_back_to_generic() {
        assert_eq!(detect_from_url("localhost:5100").name(), "generic");
        assert_eq!(detect_from_url("ghcr.io").name(), "generic"); // until Ghcr driver lands
        assert_eq!(detect_from_url("harbor.internal").name(), "generic");
    }

    #[test]
    fn driver_by_name_respects_auto_and_known_names() {
        assert_eq!(
            driver_by_name_or_auto("auto", "registry.gitlab.com")
                .unwrap()
                .name(),
            "gitlab"
        );
        assert_eq!(
            driver_by_name_or_auto("auto", "localhost:5100")
                .unwrap()
                .name(),
            "generic"
        );
        assert_eq!(
            driver_by_name_or_auto("generic", "registry.gitlab.com")
                .unwrap()
                .name(),
            "generic"
        );
        assert_eq!(
            driver_by_name_or_auto("gitlab", "localhost:5100")
                .unwrap()
                .name(),
            "gitlab"
        );
        assert!(driver_by_name_or_auto("martian", "localhost").is_err());
    }
}
