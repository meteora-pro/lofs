//! # lofs-core
//!
//! Core types and OCI-backed persistence for the **Layered Overlay File
//! System** (LOFS).
//!
//! The L0 MVP slice described in [ADR-001](../../docs/architecture/adr/ADR-001-lofs.md)
//! and [ADR-002](../../docs/architecture/adr/ADR-002-cooperative-coordination.md)
//! ships three modules:
//!
//! - [`bucket`] — bucket identity (id, name, org), TTL / size policy, serde
//! - [`error`] — unified `LofsError` + future rich `MountAdvisory` / `PushConflict`
//! - [`oci`] — OCI registry is the single source of truth — bucket identity
//!   manifests, media types, and the push/pull/list/delete client
//!
//! FUSE mount + intent-manifest coordination land in Phase 1.2+ under
//! `backend/` and `coord/` modules (to be added).

#![warn(clippy::all)]

pub mod bucket;
pub mod error;
pub mod oci;

/// Crate version (populated from `Cargo.toml` at compile time).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use bucket::{Bucket, BucketId, BucketName, BucketStatus, NewBucket};
pub use error::{LofsError, LofsResult};
pub use oci::OciRegistry;
