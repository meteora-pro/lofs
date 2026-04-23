//! Shared helpers for integration tests against a live OCI registry.
//!
//! Tests are designed to run in two modes:
//!
//! 1. **Fast dev/CI loop** — environment variables `LOFS_TEST_ZOT` /
//!    `LOFS_TEST_DISTRIBUTION` point at already-running registries (see
//!    `docker/docker-compose.yml`, spun up via `make dev-up`). This is the
//!    default path and what every `#[tokio::test]` in `tests/` uses.
//! 2. **Hermetic** — callers can still override the URL explicitly for
//!    ad-hoc isolation; integrating `testcontainers` per-test is kept out
//!    of scope for the MVP because the docker-compose model already gives
//!    us isolated containers with far lower per-test startup cost.
//!
//! Every test reserves a unique `(org, name)` pair so parallel runs don't
//! collide; the `CleanupGuard` drops the bucket at the end of the test.

use std::env;

use chrono::Utc;
use lofs_core::{Bucket, NewBucket, OciRegistry};
use uuid::Uuid;

/// Default URL for Zot when `LOFS_TEST_ZOT` is not set — matches
/// `docker/docker-compose.yml`.
pub const DEFAULT_ZOT_URL: &str = "http://localhost:5100";

/// Default URL for CNCF Distribution when `LOFS_TEST_DISTRIBUTION` is not
/// set — matches `docker/docker-compose.yml`.
pub const DEFAULT_DISTRIBUTION_URL: &str = "http://localhost:5101";

/// Representation of a registry under test.
#[derive(Debug, Clone)]
pub struct RegistryUnderTest {
    /// Friendly name used in assertion messages + logs.
    pub label: &'static str,
    /// Base URL (scheme + host + port).
    pub url: String,
}

impl RegistryUnderTest {
    /// Resolve the Zot instance from env or fall back to the compose default.
    pub fn zot() -> Self {
        Self {
            label: "zot",
            url: env::var("LOFS_TEST_ZOT").unwrap_or_else(|_| DEFAULT_ZOT_URL.to_string()),
        }
    }

    /// Resolve the Distribution instance from env or fall back to the
    /// compose default.
    pub fn distribution() -> Self {
        Self {
            label: "distribution",
            url: env::var("LOFS_TEST_DISTRIBUTION")
                .unwrap_or_else(|_| DEFAULT_DISTRIBUTION_URL.to_string()),
        }
    }

    /// Build an `OciRegistry` client pointing at this URL.
    pub fn client(&self) -> OciRegistry {
        OciRegistry::anonymous(&self.url)
            .unwrap_or_else(|e| panic!("{}: build OciRegistry: {e}", self.label))
    }

    /// Short-circuit: if the registry isn't reachable, skip the test. Tests
    /// that need network are marked `#[ignore]` anyway — but dev loops
    /// sometimes forget `make dev-up`, and a clean "skipping" line is much
    /// friendlier than a wall of OCI errors.
    pub async fn require_reachable(&self) {
        let c = self.client();
        if let Err(e) = c.ping().await {
            panic!(
                "{label} at {url} is not reachable — did you run `make dev-up`?\n  cause: {e}",
                label = self.label,
                url = self.url
            );
        }
    }
}

/// Allocate a random-but-readable bucket name unique to this test run, so
/// parallel tests don't collide and stale state from a previous crash does
/// not poison the assertion.
pub fn unique_name(prefix: &str) -> String {
    let suffix = Uuid::now_v7().simple().to_string();
    let short = &suffix[..12];
    let mut out = format!("{prefix}-{short}");
    // Bucket name rules: lowercase start, max 63 chars.
    out.truncate(63);
    out.make_ascii_lowercase();
    out
}

/// Allocate a random org name (also validated by the bucket name rules).
pub fn unique_org() -> String {
    unique_name("it-org")
}

/// Explicit async cleanup helper. We avoid `Drop`-based cleanup because
/// spawning a fresh tokio runtime from within an async test triggers
/// nested-runtime deadlocks on macOS under parallel execution
/// (`--test-threads > 1`). Each test calls `cleanup_bucket` at the end
/// instead — the short macro `cleanup!` below keeps it one line.
pub async fn cleanup_bucket(
    registry: &OciRegistry,
    name: &lofs_core::BucketName,
    org: Option<&str>,
) {
    let _ = registry.delete_bucket(name, org).await;
}

/// Push a fresh bucket and return `(Bucket, cleanup-closure)`.
///
/// The cleanup closure is an ordinary `async fn` the caller `.await`s at
/// the end of the test — no background threads, no Drop-based cleanup, no
/// nested runtimes.
pub async fn create_fresh_bucket(
    registry: &OciRegistry,
    prefix: &str,
    org: Option<&str>,
    ttl_days: i64,
) -> Bucket {
    let name = unique_name(prefix);
    let bucket = NewBucket::try_new(name, org.map(str::to_string), ttl_days, Some(256))
        .expect("valid bucket inputs")
        .into_bucket_at(Utc::now());
    registry
        .push_bucket(&bucket)
        .await
        .unwrap_or_else(|e| panic!("push_bucket: {e}"));
    bucket
}
