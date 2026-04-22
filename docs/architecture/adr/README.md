# Architecture Decision Records

Log of major design decisions for LOFS.

## Status values

| Status | Meaning |
|--------|---------|
| `proposed` | Under review — may still change |
| `accepted` | Ratified; implementation should follow |
| `deprecated` | No longer recommended but retained for history |
| `superseded` | Replaced by a newer ADR |

## Active ADRs

| ID | Title | Status |
|----|-------|--------|
| [ADR-001](ADR-001-lofs.md) | LOFS — Layered Overlay File System для AI-агентов | proposed |

## Supporting research

See [`../research/`](../research/) for deep-dives that informed these decisions:

- [RESEARCH-002](../research/RESEARCH-002-crdt-fs-space.md) — CRDT-FS пространство (why v1 CRDT model was rejected)
- [RESEARCH-003](../research/RESEARCH-003-layered-bucket-storage.md) — layered bucket storage patterns (CDC, OCI, merge)
- [RESEARCH-004](../research/RESEARCH-004-rustfs-oci-coordination.md) — RustFS + S3, OCI artifacts, multi-agent coordination
- [RESEARCH-005](../research/RESEARCH-005-rust-oci-ecosystem.md) — Rust ecosystem для OCI/container infrastructure
- [RESEARCH-006](../research/RESEARCH-006-oss-prior-art.md) — OSS prior-art + competitive landscape
