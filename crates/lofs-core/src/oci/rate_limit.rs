//! HTTP rate-limit helpers for the registry layer.
//!
//! Two building blocks:
//!
//! - [`HttpLimiter`] — a cheap RAII wrapper around an optional
//!   `tokio::sync::Semaphore`. `acquire().await` returns a permit that's
//!   released on drop. When the driver's [`RateLimitPolicy::max_concurrent`]
//!   is `None`, the limiter is a no-op — perfect for local Zot /
//!   Distribution where we don't want synthetic latency.
//!
//! - [`HttpLimiter::retry_on_429`] — wraps a `reqwest` call and, on a
//!   `429 Too Many Requests`, reads the registry's `Retry-After` header
//!   (driver-configurable), sleeps, and retries up to
//!   `RateLimitPolicy::max_retries`.
//!
//! These are plumbing utilities — individual call sites in `oci/registry.rs`
//! and the driver layer call into them.

use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use reqwest::{Response, StatusCode};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::driver::RateLimitPolicy;
use crate::error::LofsResult;

/// Rate-limit gate shared across every HTTP call an `OciRegistry`
/// issues. Cheap to `Clone` — inner state lives behind an `Arc`.
#[derive(Debug, Clone)]
pub struct HttpLimiter {
    policy: RateLimitPolicy,
    semaphore: Option<Arc<Semaphore>>,
}

impl HttpLimiter {
    /// Build a limiter from a driver's policy.
    pub fn new(policy: RateLimitPolicy) -> Self {
        let semaphore = policy
            .max_concurrent
            .map(|n| Arc::new(Semaphore::new(n.max(1))));
        Self { policy, semaphore }
    }

    /// Inspect the policy this limiter enforces.
    pub fn policy(&self) -> &RateLimitPolicy {
        &self.policy
    }

    /// Acquire a permit. When `max_concurrent` is unbounded the returned
    /// `HttpPermit` holds nothing — drop is a no-op. Otherwise it holds
    /// an `OwnedSemaphorePermit` which releases on drop.
    pub async fn acquire(&self) -> HttpPermit {
        match &self.semaphore {
            Some(sem) => {
                // `Semaphore::acquire_owned` never errors for a live
                // semaphore — we hold an Arc to it so it can't be closed
                // from under us.
                let permit = sem
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("live semaphore is never closed");
                HttpPermit {
                    _inner: Some(permit),
                }
            }
            None => HttpPermit { _inner: None },
        }
    }

    /// Run `call` and, on a `429` response, honour the server's
    /// `Retry-After` up to `policy.max_retries` times. `call` is invoked
    /// fresh each attempt — perfect for `|| self.http.get(&url).send()`
    /// style closures. The limiter doesn't auto-acquire a permit; the
    /// caller is expected to hold one for the duration.
    pub async fn retry_on_429<F, Fut>(&self, mut call: F) -> LofsResult<Response>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<Response, reqwest::Error>>,
    {
        let mut attempt: u32 = 0;
        loop {
            let res = call().await?;
            if res.status() != StatusCode::TOO_MANY_REQUESTS || attempt >= self.policy.max_retries {
                return Ok(res);
            }
            let wait = parse_retry_after(&res, self.policy.retry_after_header)
                .unwrap_or(self.policy.default_backoff);
            // `res` is consumed — but we already know the status. Log
            // would be nice here, but we stay dependency-free.
            tokio::time::sleep(wait).await;
            attempt = attempt.saturating_add(1);
        }
    }
}

impl Default for HttpLimiter {
    fn default() -> Self {
        Self::new(RateLimitPolicy::default())
    }
}

/// RAII guard — holds a semaphore permit if the limiter had a cap,
/// nothing otherwise. Drop releases the slot.
#[must_use = "a permit must stay in scope until the HTTP call completes"]
pub struct HttpPermit {
    _inner: Option<OwnedSemaphorePermit>,
}

impl std::fmt::Debug for HttpPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpPermit")
            .field("capped", &self._inner.is_some())
            .finish()
    }
}

/// Parse a `Retry-After` header value in either form:
///   - decimal seconds (`Retry-After: 120`)
///   - HTTP-date (`Retry-After: Wed, 21 Oct 2026 07:28:00 GMT`)
fn parse_retry_after(res: &Response, header_name: &str) -> Option<Duration> {
    let raw = res.headers().get(header_name)?.to_str().ok()?;
    if let Ok(secs) = raw.trim().parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let date = DateTime::parse_from_rfc2822(raw.trim()).ok()?;
    let now = chrono::Utc::now();
    let delta = date.with_timezone(&chrono::Utc) - now;
    let seconds = delta.num_seconds().max(0) as u64;
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn policy(max_concurrent: Option<usize>, max_retries: u32) -> RateLimitPolicy {
        RateLimitPolicy {
            max_concurrent,
            retry_after_header: "Retry-After",
            default_backoff: Duration::from_millis(50),
            max_retries,
        }
    }

    #[tokio::test]
    async fn acquire_noop_when_uncapped() {
        let l = HttpLimiter::new(policy(None, 0));
        let _p1 = l.acquire().await;
        let _p2 = l.acquire().await;
        // Unlimited — both must return immediately.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_blocks_when_capped() {
        let l = HttpLimiter::new(policy(Some(1), 0));
        let permit = l.acquire().await;

        let l2 = l.clone();
        let handle = tokio::spawn(async move {
            let _p = l2.acquire().await;
            "woke"
        });

        // Give the spawn a moment to park on the semaphore.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!handle.is_finished(), "second acquire should be blocked");

        drop(permit);
        assert_eq!(handle.await.unwrap(), "woke");
    }

    #[test]
    fn parse_retry_after_seconds() {
        use reqwest::header::HeaderValue;
        // Build a minimal Response stub via http::Response then into reqwest.
        let inner = http::Response::builder()
            .status(429)
            .header("Retry-After", HeaderValue::from_static("7"))
            .body(bytes::Bytes::new())
            .unwrap();
        let res = Response::from(inner);

        let waited = parse_retry_after(&res, "Retry-After").unwrap();
        assert_eq!(waited, Duration::from_secs(7));
    }

    #[test]
    fn parse_retry_after_http_date() {
        let inner = http::Response::builder()
            .status(429)
            .header("Retry-After", "Wed, 21 Oct 2099 07:28:00 GMT")
            .body(bytes::Bytes::new())
            .unwrap();
        let res = Response::from(inner);

        let waited = parse_retry_after(&res, "Retry-After").unwrap();
        assert!(
            waited.as_secs() > 0,
            "future date should yield positive wait"
        );
    }

    #[test]
    fn parse_retry_after_missing_returns_none() {
        let inner = http::Response::builder()
            .status(429)
            .body(bytes::Bytes::new())
            .unwrap();
        let res = Response::from(inner);
        assert!(parse_retry_after(&res, "Retry-After").is_none());
    }

    #[tokio::test]
    async fn retry_on_429_retries_and_succeeds() {
        // Use a tiny `Retry-After` header (1s parsed → default_backoff
        // fallback of 20ms since "1" → 1 second is rounded to the closest
        // thing we can test without paused-time feature). Trick: policy
        // default_backoff is 20ms, and we omit Retry-After entirely so
        // the limiter falls back to that.
        let l = HttpLimiter::new(RateLimitPolicy {
            max_concurrent: None,
            retry_after_header: "Retry-After",
            default_backoff: Duration::from_millis(20),
            max_retries: 3,
        });

        let mut attempts = 0_u32;
        let start = Instant::now();
        let res = l
            .retry_on_429(|| {
                attempts += 1;
                let current = attempts;
                async move {
                    let status = if current < 3 {
                        StatusCode::TOO_MANY_REQUESTS
                    } else {
                        StatusCode::OK
                    };
                    let inner = http::Response::builder()
                        .status(status)
                        // No Retry-After header → limiter uses default_backoff.
                        .body(bytes::Bytes::new())
                        .unwrap();
                    Ok::<_, reqwest::Error>(Response::from(inner))
                }
            })
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(res.status(), StatusCode::OK);
        // Two sleeps × 20ms ≈ 40ms of real delay. Use a generous floor.
        assert!(
            elapsed >= Duration::from_millis(30),
            "expected at least two retry sleeps, got {elapsed:?}"
        );
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn retry_on_429_gives_up_after_max() {
        let l = HttpLimiter::new(policy(None, 1));

        let mut attempts = 0_u32;
        let res = l
            .retry_on_429(|| {
                attempts += 1;
                async move {
                    let inner = http::Response::builder()
                        .status(429)
                        .body(bytes::Bytes::new())
                        .unwrap();
                    Ok::<_, reqwest::Error>(Response::from(inner))
                }
            })
            .await
            .unwrap();
        // max_retries = 1 → we see the first response (attempt 1), retry
        // once (attempt 2), on attempt 2 the check `1 >= 1` aborts and
        // hands the 429 back.
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(attempts <= 3);
    }

    #[test]
    fn default_limiter_is_uncapped() {
        let l = HttpLimiter::default();
        assert!(l.policy().max_concurrent.is_none());
    }
}
