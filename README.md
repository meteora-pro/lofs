# lofs — Layered Overlay File System for AI agents

[English](README.md) | [Русский](README.ru.md)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-concept-orange.svg)](#status)

**LOFS** is a lightweight, rootless, OCI-backed shared **workspace** primitive for multi-agent AI systems. It gives agents a real POSIX filesystem they can mount, work in, and commit back as immutable layers — with support for fork/merge, ephemeral TTL, and LLM-driven merge of concurrent changes.

Think of it as **`git worktree` + `docker image layer` + `object storage` — but designed specifically for the handoff, fan-out/fan-in and checkpoint patterns that multi-agent systems need.** The **flat filesystem** layer (`/mnt/lofs/<session>`) is what the agent sees; under the hood each commit is an OCI layer you can push to any Docker-compatible registry (Zot, Harbor, GHCR).

> **Status: concept / infrastructure scaffold.** Not usable yet. Design documents are at [`docs/architecture/adr/`](docs/architecture/adr/). First working build: L0 (`lofs.create / list / mount / unmount`) — targeted for 2026-Q2.

## Family: LOKB ↔ LOFS

LOFS pairs with **[LOKB](https://github.com/meteora-pro/lokb)** — Local Offline Knowledge Base. They cover the two complementary tiers of agent state:

| | **LOKB** | **LOFS** |
|---|---|---|
| **Stores** | Permanent knowledge (notes, facts, past work) | Ephemeral working context (task files, artifacts) |
| **Lifetime** | Years | Hours – days – weeks |
| **Network** | Offline-first | Online by design (OCI registry, sync) |
| **Structure** | Tantivy FTS + vector embeddings + graph | Files + content-addressable snapshots + forks |
| **Git analogy** | Obsidian vault / zettelkasten | Git worktree + stash |

Agent loop: `lokb.search(...)` → "what I know" → `lofs.mount(rw, ...)` → "work on it" → `lofs.unmount(commit)` → optionally `lokb.import(...)` → "remember the conclusion".

## Why it exists

Mature AI coding agents need a shared state primitive that is:

- **Ephemeral** — bucket with explicit TTL, not a long-lived Git repo that fills up forever.
- **Fork-able** — a second agent can branch off the current state without blocking the first.
- **Mergeable** — when parallel forks converge, conflicts are resolved by an LLM *as part of the MCP surface* (not as a separate CLI tool).
- **Agent-native** — declared intent, heartbeat lifecycle, rich "who is touching what" hints in error messages.
- **File-level, not record-level** — mixed content (code + binaries + screenshots + CSVs + model dumps) is first-class.
- **Standard-storage-backed** — every snapshot is an OCI artifact you can push to Zot/Harbor/GHCR and sign with cosign. No custom registry.

Nothing in the OSS landscape (mid-2026) covers all of this in one place. The closest neighbours are documented in [RESEARCH-006](docs/architecture/research/RESEARCH-006-oss-prior-art.md).

## What makes it different

| Feature | Typical agent FS (ctxfs, AgentFS, Fast.io) | Git-for-data (lakeFS, Pachyderm, Dolt) | **LOFS** |
|---|---|---|---|
| Ephemeral TTL | ⚠️ | ❌ | ✅ |
| Fork / merge | ❌ | ✅ (but heavy) | ✅ (lightweight, agent-scale) |
| Agent-native (MCP + intent) | ⚠️ | ❌ | ✅ |
| LLM-driven merge as first-class | ❌ | ❌ | ✅ |
| OCI-artifact wire format | ❌ | ❌ | ✅ (interop with Zot/Harbor, cosign) |

## L0 API — 4 MCP tools

At launch, the entire surface agents see is these four tools. Everything else is an optional evolution we add only when real telemetry demands it.

```
lofs.create({ name, ttl_days, size_limit_mb }) -> bucket_id

lofs.list({ org?, filter? }) -> BucketInfo[]     # includes current lock, active forks, size, hints

lofs.mount({
  bucket_id,
  mode: "ro" | "rw" | "fork",
  purpose: string,              # free-text — why agent needs it
  expected_duration_sec: u32,
  labels?: {...},
}) -> { mount_path, session_id } | MountError { holder, overlap_analysis, hints[] }

lofs.unmount({
  session_id,
  action: "commit" | "discard" | "merge",
  message?,
  resolutions?: [...],          # only on second call when previous unmount returned conflicts
}) -> { new_snapshot_id } | MergeConflicts { ... }
```

`rw` mode takes an exclusive lock. If it's already held, the caller receives a **rich error with the holder's declared purpose, expected release time, LLM-generated overlap analysis, and a list of hints** (wait / fork / try ro / split scope). No silent failures, no hidden locks — the agent has every piece of information a human reviewer would want.

See [ADR-001-lofs.md](docs/architecture/adr/ADR-001-lofs.md) for the full design.

## Evolution roadmap (L1+)

Added **only when metrics show they're needed**:

| Layer | Trigger | Adds |
|-------|---------|------|
| L1 | fork+merge workflow in use | LLM-driven merge ladder (Identical → Mergiraf → LLM → Human) |
| L2 | storage cost > threshold | Per-file BLAKE3 dedup + reference counting |
| L3 | dedup ratio < 2× | Content-defined chunking (FastCDC) + pack files (zstd:chunked) |
| L4 | reads of partial large files | Lazy-mount via SOCI-style zTOC + Range-GET |
| L5 | long-lived archives | Tiered hot/cold storage (self-hosted hot + S3 Glacier) |
| L6 | agent confusion in long history | AI-generated cold-tier summaries + Ed25519 signatures |
| L7 | first auto-merge incident | HITL approval policy per sensitive path |

## Under the hood

```
┌────────────────────────────────────────────────────────────────────┐
│                        AGENT (Claude Code, Codex, …)                │
│                                                                     │
│     lofs.mount ──> /mnt/lofs/<session>  (regular POSIX path)        │
│                    agent: cat, grep, cargo, git, python …           │
└───────────────────────────────┬────────────────────────────────────┘
                                │ MCP
┌───────────────────────────────▼────────────────────────────────────┐
│                          lofs-daemon (Rust)                         │
│                                                                     │
│  libfuse-fs + ocirender     Postgres lock    oci-client / Zot        │
│  (rootless overlay mount)  (metadata)       (artifact push/pull)    │
└───────────────────────────────┬────────────────────────────────────┘
                                │
                                ▼
          OCI-compatible registry (Zot / Harbor / GHCR / GitLab)
```

**Key libraries** (see [RESEARCH-005](docs/architecture/research/RESEARCH-005-rust-oci-ecosystem.md) for detail):

- [`oci-spec`](https://github.com/youki-dev/oci-spec-rs) + [`oci-client`](https://github.com/oras-project/rust-oci-client) — OCI manifest + registry
- [`libfuse-fs`](https://crates.io/crates/libfuse-fs) + [`fuser`](https://github.com/cberner/fuser) — rootless userspace overlay
- [`ocirender`](https://edera.dev/stories/rendering-oci-images-the-right-way-introducing-ocirender) — streaming OCI layer merge with whiteouts
- [`fastcdc`](https://crates.io/crates/fastcdc) + [`zstd`](https://github.com/gyscos/zstd-rs) — chunking + compression (L3+)
- [`nix`](https://docs.rs/nix) + [`caps`](https://github.com/lucab/caps-rs) — user namespaces + Linux capabilities
- Sync `tar` crate — **never** tokio-tar (CVE-2025-62518)

## Status

This repository is a concept scaffold. Design is in review. Not recommended for any real use yet. Implementation work is tracked via GitHub issues once the architecture is accepted.

If you're curious about the thinking that led here, read the ADRs and RESEARCH docs in [docs/architecture/](docs/architecture/).

## Documentation

- [ADR-001: LOFS design](docs/architecture/adr/ADR-001-lofs.md) — full L0/L1 architecture
- [Research directory](docs/architecture/research/) — six deep-dives on CRDT-FS space, layered storage, coordination, Rust ecosystem, prior art
- [Contributing](CONTRIBUTING.md)

## License

MIT — see [LICENSE](LICENSE).

## Part of Meteora

- **[LOKB](https://github.com/meteora-pro/lokb)** — Local Offline Knowledge Base (permanent memory tier)
- **[devboy-tools](https://github.com/meteora-pro/devboy-tools)** — DevBoy MCP server with plugin system
- **lofs** (this repo) — ephemeral shared workspace tier
