//! Bucket domain types for LOFS.
//!
//! A bucket is the agent-scoped ephemeral workspace primitive: name + TTL +
//! size limit + status. OCI layers / snapshots / mount sessions hang off
//! this identity; they are modelled in sibling modules (Phase 1.2+).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

use crate::error::{LofsError, LofsResult};

/// Minimum bucket TTL in days. A TTL of `0` would make buckets disappear
/// during the same session, which never matches an agent's workflow.
pub const MIN_TTL_DAYS: i64 = 1;

/// Maximum bucket TTL in days. Anything longer belongs in a long-lived
/// artefact store, not an ephemeral workspace.
pub const MAX_TTL_DAYS: i64 = 365;

/// Default size limit applied when the caller omits it (1 GiB).
pub const DEFAULT_SIZE_LIMIT_MB: i64 = 1024;

/// Maximum allowed size limit (64 GiB). Guards against runaway storage cost.
pub const MAX_SIZE_LIMIT_MB: i64 = 64 * 1024;

/// Minimum bucket name length (inclusive).
pub const MIN_NAME_LEN: usize = 2;

/// Maximum bucket name length (inclusive). Chosen so that name fits inside
/// OCI tag constraints (`[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`) with headroom.
pub const MAX_NAME_LEN: usize = 63;

/// Content-addressable identifier for a bucket. UUIDv7 — time-ordered so
/// sorted listings are chronological by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BucketId(pub Uuid);

impl BucketId {
    /// Allocate a fresh UUIDv7-backed bucket id.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Return the underlying UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for BucketId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BucketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for BucketId {
    type Err = LofsError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s)
            .map(BucketId)
            .map_err(|e| LofsError::Other(format!("invalid bucket id `{s}`: {e}")))
    }
}

/// Validated bucket name. Construct via [`BucketName::new`] — invalid input
/// short-circuits with [`LofsError::InvalidName`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BucketName(String);

impl BucketName {
    /// Validate + wrap an owned string as a bucket name.
    ///
    /// Rules:
    /// - length in `[MIN_NAME_LEN, MAX_NAME_LEN]`
    /// - first char: lowercase ASCII letter or digit
    /// - remaining chars: lowercase ASCII alnum, `-`, `_`
    pub fn new(input: impl Into<String>) -> LofsResult<Self> {
        let s = input.into();
        let len = s.len();
        if !(MIN_NAME_LEN..=MAX_NAME_LEN).contains(&len) {
            return Err(LofsError::InvalidName {
                name: s,
                reason: format!("length must be {MIN_NAME_LEN}..={MAX_NAME_LEN} chars"),
            });
        }
        let mut chars = s.chars();
        let first = chars.next().expect("length checked above");
        if !first.is_ascii_alphanumeric() || first.is_ascii_uppercase() {
            return Err(LofsError::InvalidName {
                name: s,
                reason: "first char must be [a-z0-9]".into(),
            });
        }
        for c in chars {
            let ok = (c.is_ascii_alphanumeric() && !c.is_ascii_uppercase()) || c == '-' || c == '_';
            if !ok {
                return Err(LofsError::InvalidName {
                    name: s,
                    reason: format!("invalid char `{c}`; allowed: [a-z0-9-_]"),
                });
            }
        }
        Ok(Self(s))
    }

    /// Borrow the validated name as `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BucketName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for BucketName {
    type Error = LofsError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<BucketName> for String {
    fn from(value: BucketName) -> Self {
        value.0
    }
}

impl FromStr for BucketName {
    type Err = LofsError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.to_owned())
    }
}

/// Bucket lifecycle state. The MVP surfaces only `Active` and `Expired`;
/// `Deleted` is reserved for soft-delete once GC lands (Phase 1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BucketStatus {
    /// Bucket is usable.
    Active,
    /// TTL elapsed — reads still allowed, writes rejected; waits for GC.
    Expired,
    /// Soft-deleted by owner; awaiting registry GC.
    Deleted,
}

impl BucketStatus {
    /// Stable on-disk / wire representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Deleted => "deleted",
        }
    }
}

impl fmt::Display for BucketStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BucketStatus {
    type Err = LofsError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "active" => Self::Active,
            "expired" => Self::Expired,
            "deleted" => Self::Deleted,
            other => return Err(LofsError::Other(format!("unknown bucket status `{other}`"))),
        })
    }
}

/// Inputs required to allocate a new bucket. All fields are validated at
/// construction; invalid input short-circuits with [`LofsError`].
#[derive(Debug, Clone)]
pub struct NewBucket {
    /// Validated bucket name.
    pub name: BucketName,
    /// Optional logical owner (org/team/user). `None` means personal scope.
    pub org: Option<String>,
    /// Days from `created_at` until the bucket auto-expires.
    pub ttl_days: i64,
    /// Cap on total on-registry blob size for this bucket.
    pub size_limit_mb: i64,
}

impl NewBucket {
    /// Validate inputs and assemble a `NewBucket`.
    pub fn try_new(
        name: impl Into<String>,
        org: Option<String>,
        ttl_days: i64,
        size_limit_mb: Option<i64>,
    ) -> LofsResult<Self> {
        let name = BucketName::new(name)?;
        if !(MIN_TTL_DAYS..=MAX_TTL_DAYS).contains(&ttl_days) {
            return Err(LofsError::InvalidTtl {
                ttl_days,
                reason: format!("must be {MIN_TTL_DAYS}..={MAX_TTL_DAYS}"),
            });
        }
        let size_limit_mb = size_limit_mb.unwrap_or(DEFAULT_SIZE_LIMIT_MB);
        if size_limit_mb <= 0 || size_limit_mb > MAX_SIZE_LIMIT_MB {
            return Err(LofsError::InvalidSizeLimit {
                size_limit_mb,
                reason: format!("must be 1..={MAX_SIZE_LIMIT_MB}"),
            });
        }
        Ok(Self {
            name,
            org,
            ttl_days,
            size_limit_mb,
        })
    }

    /// Materialise into a full `Bucket` anchored at the given creation time.
    pub fn into_bucket_at(self, created_at: DateTime<Utc>) -> Bucket {
        let expires_at = created_at + Duration::days(self.ttl_days);
        Bucket {
            id: BucketId::new(),
            name: self.name,
            org: self.org,
            ttl_days: self.ttl_days,
            size_limit_mb: self.size_limit_mb,
            created_at,
            expires_at,
            status: BucketStatus::Active,
        }
    }
}

/// Fully-materialised bucket record (as stored + returned by the metadata
/// store).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    /// Time-ordered identifier.
    pub id: BucketId,
    /// Validated bucket name.
    pub name: BucketName,
    /// Optional logical owner.
    pub org: Option<String>,
    /// TTL in days (the absolute deadline is in `expires_at`).
    pub ttl_days: i64,
    /// Size cap in megabytes.
    pub size_limit_mb: i64,
    /// Creation timestamp (UTC).
    pub created_at: DateTime<Utc>,
    /// Absolute expiry timestamp (UTC). Past this point the bucket flips
    /// to [`BucketStatus::Expired`] on next access.
    pub expires_at: DateTime<Utc>,
    /// Current lifecycle state.
    pub status: BucketStatus,
}

impl Bucket {
    /// Return `true` if the bucket's TTL has elapsed relative to `now`.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Remaining lifetime (positive = time left, negative = already expired).
    pub fn remaining_at(&self, now: DateTime<Utc>) -> Duration {
        self.expires_at - now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_name_accepts_canonical_input() {
        let n = BucketName::new("dev-666-research").unwrap();
        assert_eq!(n.as_str(), "dev-666-research");
    }

    #[test]
    fn bucket_name_rejects_uppercase() {
        let e = BucketName::new("Foo").unwrap_err();
        assert!(matches!(e, LofsError::InvalidName { .. }));
    }

    #[test]
    fn bucket_name_rejects_leading_dash() {
        let e = BucketName::new("-foo").unwrap_err();
        assert!(matches!(e, LofsError::InvalidName { .. }));
    }

    #[test]
    fn bucket_name_rejects_too_short() {
        let e = BucketName::new("a").unwrap_err();
        assert!(matches!(e, LofsError::InvalidName { .. }));
    }

    #[test]
    fn bucket_name_rejects_too_long() {
        let s = "a".repeat(MAX_NAME_LEN + 1);
        let e = BucketName::new(s).unwrap_err();
        assert!(matches!(e, LofsError::InvalidName { .. }));
    }

    #[test]
    fn new_bucket_rejects_out_of_range_ttl() {
        let e = NewBucket::try_new("ok-name", None, MAX_TTL_DAYS + 1, None).unwrap_err();
        assert!(matches!(e, LofsError::InvalidTtl { .. }));

        let e = NewBucket::try_new("ok-name", None, 0, None).unwrap_err();
        assert!(matches!(e, LofsError::InvalidTtl { .. }));
    }

    #[test]
    fn new_bucket_rejects_invalid_size_limit() {
        let e = NewBucket::try_new("ok-name", None, 1, Some(0)).unwrap_err();
        assert!(matches!(e, LofsError::InvalidSizeLimit { .. }));

        let e = NewBucket::try_new("ok-name", None, 1, Some(MAX_SIZE_LIMIT_MB + 1)).unwrap_err();
        assert!(matches!(e, LofsError::InvalidSizeLimit { .. }));
    }

    #[test]
    fn new_bucket_applies_default_size_limit() {
        let nb = NewBucket::try_new("ok-name", None, 7, None).unwrap();
        assert_eq!(nb.size_limit_mb, DEFAULT_SIZE_LIMIT_MB);
    }

    #[test]
    fn into_bucket_at_sets_expiry_from_ttl() {
        let now = Utc::now();
        let b = NewBucket::try_new("ok-name", None, 7, Some(512))
            .unwrap()
            .into_bucket_at(now);
        assert_eq!(b.expires_at - b.created_at, Duration::days(7));
        assert_eq!(b.status, BucketStatus::Active);
        assert!(!b.is_expired_at(now));
        assert!(b.is_expired_at(now + Duration::days(7)));
    }

    #[test]
    fn bucket_serde_roundtrip() {
        let b = NewBucket::try_new("round-trip", Some("meteora".into()), 3, Some(256))
            .unwrap()
            .into_bucket_at(Utc::now());
        let json = serde_json::to_string(&b).unwrap();
        let decoded: Bucket = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name.as_str(), "round-trip");
        assert_eq!(decoded.org.as_deref(), Some("meteora"));
    }

    #[test]
    fn bucket_id_is_time_ordered() {
        let a = BucketId::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = BucketId::new();
        assert!(a.as_uuid() < b.as_uuid(), "UUIDv7 should be monotonic");
    }

    #[test]
    fn bucket_status_parses_back_and_forth() {
        for s in [
            BucketStatus::Active,
            BucketStatus::Expired,
            BucketStatus::Deleted,
        ] {
            let round: BucketStatus = s.as_str().parse().unwrap();
            assert_eq!(round, s);
        }
    }
}
