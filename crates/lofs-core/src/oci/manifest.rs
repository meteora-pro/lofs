//! Bucket identity manifest — OCI wire representation for L0 `lofs.create`.
//!
//! The L0 bucket manifest is an OCI artifact-style image manifest whose
//! config blob carries the canonical `Bucket` JSON and whose annotations
//! duplicate identity for cheap listing (no need to pull the config for
//! `lofs.list`) and for disaster recovery (daemon rebuilds state from
//! annotations alone if the config blob is evicted).
//!
//! Layers are **empty** in L0 — a bucket has no content until the first
//! `unmount commit` (Phase 1.2).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use oci_client::client::{Config, ImageLayer};
use oci_client::manifest::{OciDescriptor, OciImageManifest};
use serde::{Deserialize, Serialize};

use super::media_types::{ANNOTATION_NS, BUCKET_CONFIG_V1, CONFIG_MEDIA_TYPE};
use crate::bucket::{Bucket, BucketId, BucketName, BucketStatus};
use crate::error::{LofsError, LofsResult};

/// Canonical JSON representation of a bucket that lives as the `config` blob
/// of the manifest. Field ordering is chosen to match the `Bucket` struct so
/// round-trips are byte-stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketConfig {
    /// Schema version — bump on any breaking change to this blob.
    pub schema_version: u32,
    /// Identity of the bucket.
    pub bucket: Bucket,
}

impl BucketConfig {
    /// Current schema version emitted by this crate.
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;

    /// Wrap a `Bucket` in the current schema version.
    pub fn new(bucket: Bucket) -> Self {
        Self {
            schema_version: Self::CURRENT_SCHEMA_VERSION,
            bucket,
        }
    }

    /// Serialize to the on-registry blob bytes.
    pub fn to_bytes(&self) -> LofsResult<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Deserialize from on-registry blob bytes.
    pub fn from_bytes(bytes: &[u8]) -> LofsResult<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

/// Build the `oci-client::Config` payload representing a bucket.
///
/// Note: we publish the blob under the **standard** OCI config media type
/// (`application/vnd.oci.image.config.v1+json`) rather than our
/// vendor-prefixed one — GitLab CR, Harbor, and some others only accept a
/// closed set of `config.mediaType` values. LOFS identity is carried by
/// the manifest-level `artifactType` (set in [`build_manifest`]) and by
/// annotations, which every registry we tested accepts.
pub fn build_config(bucket: &Bucket) -> LofsResult<Config> {
    let body = BucketConfig::new(bucket.clone()).to_bytes()?;
    Ok(Config {
        data: body.into(),
        media_type: CONFIG_MEDIA_TYPE.to_string(),
        annotations: None,
    })
}

/// Compose the full OCI manifest for a bucket, with identity annotations
/// duplicated onto the manifest itself.
///
/// We deliberately do **not** set `artifactType` to our vendor value.
/// OCI 1.1 says registries SHOULD accept any artifactType, but GitLab
/// Container Registry currently rejects manifests with unknown vendor
/// artifact types (seen as `MANIFEST_INVALID: unknown media type`). The
/// LOFS identity is fully recoverable from the `pro.meteora.lofs.*`
/// annotations, so we don't need artifactType for correctness.
pub fn build_manifest(bucket: &Bucket, config: &Config) -> OciImageManifest {
    let layers: &[ImageLayer] = &[];
    OciImageManifest::build(layers, config, Some(bucket_annotations(bucket)))
}

/// Identity-only annotations (what we duplicate on both the manifest and
/// — optionally — on individual descriptors). Keeping them stable is what
/// lets `lofs.list` avoid pulling the config blob for every bucket.
pub fn bucket_annotations(bucket: &Bucket) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let ns = ANNOTATION_NS;
    m.insert(format!("{ns}.kind"), "bucket".into());
    m.insert(format!("{ns}.bucket_id"), bucket.id.to_string());
    m.insert(format!("{ns}.name"), bucket.name.to_string());
    if let Some(org) = &bucket.org {
        m.insert(format!("{ns}.org"), org.clone());
    }
    m.insert(format!("{ns}.ttl_days"), bucket.ttl_days.to_string());
    m.insert(
        format!("{ns}.size_limit_mb"),
        bucket.size_limit_mb.to_string(),
    );
    m.insert(format!("{ns}.created_at"), bucket.created_at.to_rfc3339());
    m.insert(format!("{ns}.expires_at"), bucket.expires_at.to_rfc3339());
    m.insert(format!("{ns}.status"), bucket.status.to_string());
    m
}

/// Reconstruct a `Bucket` from manifest-level annotations alone — used by
/// `lofs.list` which wants to avoid pulling the config blob per entry.
/// Missing or malformed annotations fall back to the config-blob path (see
/// `registry::pull_bucket` which calls this first and fetches the config on
/// failure).
pub fn bucket_from_annotations(annotations: &BTreeMap<String, String>) -> LofsResult<Bucket> {
    let ns = ANNOTATION_NS;
    let get = |k: &str| -> LofsResult<&String> {
        annotations
            .get(&format!("{ns}.{k}"))
            .ok_or_else(|| LofsError::Registry(format!("annotation `{ns}.{k}` missing")))
    };

    let id: BucketId = get("bucket_id")?
        .parse()
        .map_err(|e| LofsError::Registry(format!("bad bucket_id annotation: {e}")))?;
    let name = BucketName::new(get("name")?.clone())?;
    let org = annotations.get(&format!("{ns}.org")).cloned();
    let ttl_days: i64 = get("ttl_days")?
        .parse()
        .map_err(|e| LofsError::Registry(format!("bad ttl_days annotation: {e}")))?;
    let size_limit_mb: i64 = get("size_limit_mb")?
        .parse()
        .map_err(|e| LofsError::Registry(format!("bad size_limit_mb annotation: {e}")))?;
    let created_at = parse_rfc3339(get("created_at")?, "created_at")?;
    let expires_at = parse_rfc3339(get("expires_at")?, "expires_at")?;
    let status: BucketStatus = get("status")?
        .parse()
        .map_err(|e| LofsError::Registry(format!("bad status annotation: {e}")))?;

    Ok(Bucket {
        id,
        name,
        org,
        ttl_days,
        size_limit_mb,
        created_at,
        expires_at,
        status,
    })
}

fn parse_rfc3339(value: &str, field: &'static str) -> LofsResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| LofsError::Registry(format!("bad {field} annotation: {e}")))
}

/// Manifest descriptor summary that `lofs.list` uses to decide how to
/// resolve each catalog entry. Not serialized anywhere — purely internal.
#[derive(Debug, Clone)]
pub struct ListedManifest {
    /// Registry-qualified tag reference (e.g. `lofs/meteora/foo:latest`).
    pub tag_ref: String,
    /// Manifest digest (`sha256:...`).
    pub digest: String,
    /// OCI media type of the manifest.
    pub media_type: String,
    /// Top-level annotations as published by the registry.
    pub annotations: BTreeMap<String, String>,
}

impl ListedManifest {
    /// True when this is a LOFS bucket artifact (guards against random
    /// repositories that might share the namespace).
    pub fn is_bucket(&self) -> bool {
        matches!(
            self.annotations.get(&format!("{ANNOTATION_NS}.kind")),
            Some(kind) if kind == "bucket"
        )
    }
}

/// Convenience helper used by tests and the CLI to normalise a bucket +
/// config into the exact (config, manifest) pair we push.
pub fn build_pair(bucket: &Bucket) -> LofsResult<(Config, OciImageManifest)> {
    let cfg = build_config(bucket)?;
    let mf = build_manifest(bucket, &cfg);
    Ok((cfg, mf))
}

/// Size descriptor helper for building a config descriptor from raw bytes
/// (used when we pulled the config via `pull_blob`).
pub fn config_descriptor(digest: String, size: i64) -> OciDescriptor {
    OciDescriptor {
        media_type: CONFIG_MEDIA_TYPE.to_string(),
        digest,
        size,
        urls: None,
        annotations: None,
    }
}

// Silence unused-import warnings when the `BUCKET_CONFIG_V1` alias is only
// referenced externally (kept for backwards-compat).
#[allow(dead_code)]
const _: &str = BUCKET_CONFIG_V1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bucket::NewBucket;

    fn sample_bucket() -> Bucket {
        NewBucket::try_new("sample-bucket", Some("meteora".into()), 7, Some(256))
            .unwrap()
            .into_bucket_at(Utc::now())
    }

    #[test]
    fn config_roundtrip_is_byte_stable() {
        let b = sample_bucket();
        let wire = BucketConfig::new(b.clone()).to_bytes().unwrap();
        let back = BucketConfig::from_bytes(&wire).unwrap();
        assert_eq!(back.schema_version, BucketConfig::CURRENT_SCHEMA_VERSION);
        assert_eq!(back.bucket.id, b.id);
        assert_eq!(back.bucket.name.as_str(), b.name.as_str());
    }

    #[test]
    fn annotations_capture_identity() {
        let b = sample_bucket();
        let a = bucket_annotations(&b);
        assert_eq!(
            a.get("pro.meteora.lofs.kind").map(String::as_str),
            Some("bucket")
        );
        assert_eq!(
            a.get("pro.meteora.lofs.name").map(String::as_str),
            Some("sample-bucket")
        );
        assert_eq!(
            a.get("pro.meteora.lofs.org").map(String::as_str),
            Some("meteora")
        );
        assert_eq!(
            a.get("pro.meteora.lofs.ttl_days").map(String::as_str),
            Some("7")
        );
        assert_eq!(
            a.get("pro.meteora.lofs.size_limit_mb").map(String::as_str),
            Some("256")
        );
    }

    #[test]
    fn annotations_roundtrip_to_bucket() {
        let b = sample_bucket();
        let a = bucket_annotations(&b);
        let decoded = bucket_from_annotations(&a).unwrap();
        assert_eq!(decoded.id, b.id);
        assert_eq!(decoded.name.as_str(), b.name.as_str());
        assert_eq!(decoded.org.as_deref(), Some("meteora"));
        assert_eq!(decoded.ttl_days, 7);
    }

    #[test]
    fn annotations_without_org_roundtrip() {
        let b = NewBucket::try_new("nop", None, 1, None)
            .unwrap()
            .into_bucket_at(Utc::now());
        let a = bucket_annotations(&b);
        let decoded = bucket_from_annotations(&a).unwrap();
        assert_eq!(decoded.org, None);
    }

    #[test]
    fn missing_annotation_errors_clearly() {
        let mut a = bucket_annotations(&sample_bucket());
        a.remove("pro.meteora.lofs.name");
        let err = bucket_from_annotations(&a).unwrap_err();
        assert!(matches!(err, LofsError::Registry(msg) if msg.contains("name")));
    }

    #[test]
    fn listed_manifest_recognises_bucket() {
        let mut anns = BTreeMap::new();
        anns.insert("pro.meteora.lofs.kind".into(), "bucket".into());
        let m = ListedManifest {
            tag_ref: "foo".into(),
            digest: "sha256:0".into(),
            media_type: "x".into(),
            annotations: anns,
        };
        assert!(m.is_bucket());
    }

    #[test]
    fn listed_manifest_rejects_other_kinds() {
        let mut anns = BTreeMap::new();
        anns.insert("pro.meteora.lofs.kind".into(), "snapshot".into());
        let m = ListedManifest {
            tag_ref: "foo".into(),
            digest: "sha256:0".into(),
            media_type: "x".into(),
            annotations: anns,
        };
        assert!(!m.is_bucket());
    }
}
