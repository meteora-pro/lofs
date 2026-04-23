//! Media-type constants for LOFS OCI artifacts.
//!
//! These vendor-prefixed strings follow the OCI 1.1 artifact pattern. Zot,
//! Distribution, Harbor, GHCR, and GitLab Container Registry all accept
//! unknown media types as per the Distribution spec — our tests verify that
//! end-to-end on both Zot and Distribution.

/// Media type reported for the config blob in the **pushed manifest**.
/// We use the standard OCI config media type rather than a vendor-prefixed
/// one — GitLab Container Registry (and a few others) only accept a
/// closed allow-list of config types, and the LOFS identity still
/// round-trips through manifest annotations + the `artifactType` field
/// below.
pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";

/// OCI 1.1 `artifactType` we stamp onto every bucket manifest. This is
/// how we identify our own artifacts on registries that accept any
/// vendor artifact type but tighten `config.mediaType` (GitLab, Harbor).
pub const BUCKET_ARTIFACT_TYPE: &str = "application/vnd.meteora.lofs.bucket.v1+json";

/// Legacy alias — still exported because downstream tests reference it.
/// Points at the artifact type now.
pub const BUCKET_CONFIG_V1: &str = BUCKET_ARTIFACT_TYPE;

/// Snapshot manifest media type (will be used in Phase 1.2+ when commits
/// push actual content layers). Kept here so the constant lives in one place.
pub const SNAPSHOT_MANIFEST_V1: &str = "application/vnd.meteora.lofs.snapshot.v1+json";

/// Intent manifest media type (ADR-002 — cooperative mount coordination).
pub const INTENT_MANIFEST_V1: &str = "application/vnd.meteora.lofs.intent.v1+json";

/// Annotation key prefix — all LOFS annotations live under this namespace.
pub const ANNOTATION_NS: &str = "pro.meteora.lofs";
