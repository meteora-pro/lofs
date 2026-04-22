//! # lofs-core
//!
//! Core types, storage abstractions, and overlay/OCI primitives for the
//! **Layered Overlay File System** (LOFS).
//!
//! This crate is a **concept scaffold** — it compiles, it re-exports the
//! dependency set the real implementation will use, and it lays down the
//! top-level module skeleton from [ADR-001](../../docs/architecture/adr/ADR-001-lofs.md).
//!
//! # Surface (planned)
//!
//! The Phase-1 L0 surface is four MCP tools exposed by `lofs-mcp`:
//!
//! ```text
//! lofs.create   — allocate a new bucket with TTL + size limit
//! lofs.list     — enumerate buckets with active locks / forks / hints
//! lofs.mount    — mount a bucket as an overlay filesystem (ro | rw | fork)
//! lofs.unmount  — commit as a new OCI layer / discard / attempt merge
//! ```
//!
//! Module split inside this crate will be:
//!
//! - `bucket`  — bucket metadata, TTL, size-limit enforcement
//! - `snapshot` — content-addressable snapshot manifest (OCI-compatible)
//! - `mount`   — overlay orchestration (libfuse-fs + ocirender)
//! - `pack`    — OCI layer packaging (tar + zstd, optionally zstd:chunked)
//! - `store`   — OpenDAL-backed blob / manifest store
//! - `lock`    — Postgres-backed mount lock + heartbeat
//! - `oci`     — push/pull via `oci-client` + media-type registry
//!
//! See the ADR for the full architecture.

#![deny(missing_docs)]
#![warn(clippy::all)]

/// Placeholder API version exported while the crate is in scaffold state.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Temporary shim so the workspace builds. Replaced by the real `Bucket`
/// type in Phase 1.
pub fn hello_lofs() -> &'static str {
    "lofs — Layered Overlay File System (concept scaffold)"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_compiles() {
        assert!(hello_lofs().contains("LOFS") || hello_lofs().contains("Layered"));
        assert!(!VERSION.is_empty());
    }
}
