//! OCI-backed persistence layer for LOFS (ADR-002: OCI registry is the
//! single source of truth; no mandatory database).
//!
//! Three building blocks:
//! - [`media_types`] — stable vendor-prefixed media types (`vnd.meteora.lofs.*`).
//! - [`manifest`] — translate `Bucket` ↔ config blob + manifest annotations.
//! - [`registry`] — thin `oci-client` + `reqwest` wrapper that performs the
//!   actual push / pull / catalog / delete operations.

pub mod manifest;
pub mod media_types;
pub mod registry;

pub use manifest::{BucketConfig, ListedManifest, bucket_annotations, bucket_from_annotations};
pub use media_types::{ANNOTATION_NS, BUCKET_CONFIG_V1, INTENT_MANIFEST_V1, SNAPSHOT_MANIFEST_V1};
pub use registry::{HEAD_TAG, NAMESPACE, OciRegistry, PERSONAL_ORG_SEGMENT};
