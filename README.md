# lofs — Layered Overlay File System for AI agents

[English](README.md) | [Русский](README.ru.md)

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-concept-orange.svg)](#status)

**LOFS** is a lightweight, rootless, OCI-backed shared **workspace** primitive for multi-agent AI systems. It gives agents a real POSIX filesystem they can mount, work in, and commit back as immutable layers — with support for fork/merge, ephemeral TTL, and cooperative conflict resolution across concurrent agents.

Think of it as **`git worktree` + `docker image layer` + `object storage` — but designed specifically for the handoff, fan-out/fan-in and checkpoint patterns that multi-agent systems need.** The **flat filesystem** layer (`/mnt/lofs/<session>`) is what the agent sees; under the hood each commit is an OCI layer you can push to any Docker-compatible registry (Zot, Harbor, GHCR).

LOFS has **no mandatory database**. The OCI registry is the single source of truth — both content (layers) and coordination state (intent manifests attached via the Referrers API). Optional Redis/Postgres backends for teams that outgrow the cooperative model plug in behind a single `Coordination` trait.

> **Status: concept / infrastructure scaffold.** Not usable yet. Design documents are at [`docs/architecture/adr/`](docs/architecture/adr/). First working build: L0 (`lofs.create / list / mount / unmount`) — targeted for 2026-Q2.

## Family: LOKB ↔ LOFS

LOFS pairs with **[LOKB](https://github.com/meteora-pro/lokb)** — Local Offline Knowledge Base. They cover the two complementary tiers of agent state:

| | **LOKB** | **LOFS** |
|---|---|---|
| **Stores** | Permanent knowledge (notes, facts, past work) | Ephemeral working context (task files, artifacts) |
| **Lifetime** | Years | Hours – days – weeks |
| **Network** | Offline-first | **Offline-capable** — a local OCI registry (Zot on localhost) covers the single-host case end-to-end; remote sync is opt-in when you need to share across machines |
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
- **Offline-capable** — a local registry (Zot on localhost, or a directory-backed OCI image layout) covers the solo-developer use case with zero network; remote sync becomes opt-in when the team grows beyond one machine.

Nothing in the OSS landscape (mid-2026) covers all of this in one place. The closest neighbours are documented in [RESEARCH-006](docs/architecture/research/RESEARCH-006-oss-prior-art.md).

## What makes it different

| Feature | Typical agent FS (ctxfs, AgentFS, Fast.io) | Git-for-data (lakeFS, Pachyderm, Dolt) | **LOFS** |
|---|---|---|---|
| Ephemeral TTL | ⚠️ | ❌ | ✅ |
| Fork / merge | ❌ | ✅ (but heavy) | ✅ (lightweight, agent-scale) |
| Agent-native (MCP + intent) | ⚠️ | ❌ | ✅ |
| Cooperative multi-writer model | ❌ | ❌ | ✅ (path-scoped intents via OCI Referrers) |
| Zero-infra (OCI-only, no DB) | ❌ | ❌ (needs Postgres) | ✅ (DB is an optional extension) |
| OCI-artifact wire format | ❌ | ❌ | ✅ (interop with Zot/Harbor, cosign) |

## Three surfaces

LOFS ships as **one daemon with three user-facing surfaces**, so each consumer can use what fits best:

| Surface | Audience | Transport | When to use |
|---------|----------|-----------|-------------|
| **MCP server** (`lofs-mcp`) | LLM agents (Claude Code, Codex, Kimi, custom) | JSON-RPC over stdio / WebSocket | First-class agent UX — tool calls inside LLM harness |
| **CLI** (`lofs`) | Humans & scripts | Shell | Dev ops, debugging, CI jobs, manual inspection |
| **Agent Skills** (`skills/`) | AI-agent harnesses with skill systems | File-based spec | Drop-in best-practice patterns (handoff, fan-out, checkpoint) |

### 1. MCP server — 4 L0 tools

At launch, the entire MCP surface is these four tools. Everything else is an optional evolution we add only when real telemetry demands it.

```
lofs.create({ name, ttl_days, size_limit_mb }) -> bucket_id

lofs.list({ org?, filter? }) -> BucketInfo[]     # includes current lock, active forks, size, hints

lofs.mount({
  bucket_id,
  mode: "ro" | "rw" | "fork",
  purpose: string,              # free-text — why the agent needs it
  scope?: string[],             # path globs the agent intends to write (rw only)
  expected_duration_sec: u32,
  ack_concurrent?: bool,        # set on retry to acknowledge overlapping neighbours
}) -> { mount_path, session_id } | MountAdvisory { neighbours[], hints[] }

lofs.unmount({
  session_id,
  action: "commit" | "discard",
  message?,
  conflict_policy?: "reject" | "scope_merge" | "fork_on_conflict",
}) -> { new_snapshot_id, neighbour_snapshots[] } | PushConflict { ... }
```

LOFS uses a **cooperative** model, not a pessimistic lock. Every `mount --mode=rw` publishes an **intent manifest** attached to the bucket's `:latest` tag via the OCI Referrers API. The next `rw` mount pulls the latest state, sees any active intents, and either (a) proceeds if scopes don't overlap, (b) returns a `MountAdvisory` listing the neighbours + suggested actions (wait / narrow-scope / fork / re-call with `ack_concurrent=true`). Commit is a `git pull --rebase`-style operation: pull latest, diff against the base captured at mount, push the new layer — scope-disjoint neighbours coexist, real overlaps surface as `PushConflict` that the agent (or LLM, L1+) resolves.

No silent failures, no hidden locks — and no database required on the critical path. See [ADR-002: Cooperative Coordination](docs/architecture/adr/ADR-002-cooperative-coordination.md) for the full model.

### 2. CLI — `lofs`

Mirrors the MCP surface plus ops/dev commands. Useful for scripts, CI, and debugging.

```bash
# Bucket lifecycle
lofs create <name> --ttl 7 --size-limit 1024    # ttl days, size MB
lofs list [--org X] [--filter "..."] [--format json]
lofs stat <bucket>
lofs rm <bucket>

# Mount session
lofs mount <bucket> --mode rw --purpose "implement OAuth2" --duration 600
lofs status                                      # my active sessions
lofs unmount <session_id> --commit "message"
lofs unmount <session_id> --discard

# Registry / signing
lofs registry login <registry_url>
lofs registry push <snapshot> <ref>
lofs registry pull <ref>

# Dev / ops
lofs doctor                                      # health check: userns, fuse3, registry auth
lofs gc                                          # expire buckets, drop orphaned blobs
lofs daemon                                      # run MCP server (alias for `lofs-mcp`)
```

### 3. Agent Skills — drop-in patterns

Ready-made [Agent Skills](https://agentskills.io/specification) for common multi-agent workflows. Works with Claude Code, Kimi CLI, OpenAI Codex, and any harness that supports the Agent Skills Standard. Install with `lofs skills install` or copy from `skills/` into your project's `.claude/skills/` directory.

| Skill | What it does |
|-------|--------------|
| **lofs-handoff** | Hand off work between two agents via a dedicated bucket (A commits → B reads) |
| **lofs-fan-out** | Orchestrate N sub-agents with per-task buckets and collect results |
| **lofs-checkpoint** | Long-running task pattern: periodic commits so a crashed agent can be resumed |
| **lofs-collective** | Three-agent parallel work via forks + sequential merges back to the main bucket |
| **lofs-mount-discipline** | Best-practice guide: short rw sessions, batched local edits, explicit purpose + duration |
| **lofs-review-merge** | (L1+) Agent-review pattern for LLM-driven merge proposals |

Each skill is a self-contained markdown spec (`skill.md` + examples) that teaches an LLM when and how to use LOFS primitives for that workflow.

See [ADR-001-lofs.md](docs/architecture/adr/ADR-001-lofs.md) for the overall design, [ADR-002-cooperative-coordination.md](docs/architecture/adr/ADR-002-cooperative-coordination.md) for the OCI-only coordination model, and [IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) for the roadmap.

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
┌─────────────────────────────────────────────────────────────────────────┐
│   AGENT (Claude Code / Codex / Kimi / …)     │  HUMAN / CI scripts      │
│                                              │                           │
│   MCP tools (lofs.create/list/mount/unmount) │  lofs CLI                │
│             + Agent Skills                   │  (lofs create / mount …) │
└──────────────────────┬───────────────────────┴────────────┬──────────────┘
                       │ JSON-RPC (stdio / WS)              │ local IPC
                       ▼                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                          lofs-daemon (Rust)                              │
│                                                                          │
│  libfuse-fs + ocirender     Coordination trait    oci-client             │
│  (rootless overlay mount)   (intent manifests)    (artifact push/pull)   │
│                                     │                                    │
│                                     │ default: OciCoordination           │
│                                     │ extension: Redis / Postgres        │
└────────────────────────────────┬────┴────────────────────────────────────┘
                                 │
                                 ▼
              OCI-compatible registry (Zot / Harbor / GHCR / GitLab)
              · :latest           → head snapshot manifest
              · :intent-<sid>     → ephemeral intent manifests (subject → :latest)
              · :snap-<ts>        → historical snapshots

     ┌──────────────────────────────────────────────────────────────┐
     │   agent inside a session:                                     │
     │   /mnt/lofs/<session>/    ← a regular POSIX path              │
     │   cat, grep, cargo, git, python — everything works natively   │
     └──────────────────────────────────────────────────────────────┘
```

**Key libraries** (see [RESEARCH-005](docs/architecture/research/RESEARCH-005-rust-oci-ecosystem.md) for detail):

- [`oci-spec`](https://github.com/youki-dev/oci-spec-rs) + [`oci-client`](https://github.com/oras-project/rust-oci-client) — OCI manifest + registry
- [`libfuse-fs`](https://crates.io/crates/libfuse-fs) + [`fuser`](https://github.com/cberner/fuser) — rootless userspace overlay
- [`ocirender`](https://edera.dev/stories/rendering-oci-images-the-right-way-introducing-ocirender) — streaming OCI layer merge with whiteouts
- [`fastcdc`](https://crates.io/crates/fastcdc) + [`zstd`](https://github.com/gyscos/zstd-rs) — chunking + compression (L3+)
- [`nix`](https://docs.rs/nix) + [`caps`](https://github.com/lucab/caps-rs) — user namespaces + Linux capabilities
- Sync `tar` crate — **never** tokio-tar (CVE-2025-62518)

## Install — v0.0.1

Grab a prebuilt binary from the [latest release](https://github.com/meteora-pro/lofs/releases):

```bash
# macOS arm64 (Apple Silicon)
curl -L https://github.com/meteora-pro/lofs/releases/latest/download/lofs-macos-arm64.tar.gz \
  | tar xz && sudo mv lofs /usr/local/bin/

# Linux x86_64
curl -L https://github.com/meteora-pro/lofs/releases/latest/download/lofs-linux-x86_64.tar.gz \
  | tar xz && sudo mv lofs /usr/local/bin/

# Windows: download lofs-windows-x86_64.exe.zip, unzip, put lofs.exe on PATH.

lofs --version
```

Or build from source — `cargo build --release -p lofs-cli` (needs Rust 1.86+).

Docker image: `docker pull ghcr.io/meteora-pro/lofs/cli:v0.0.1` (multi-arch linux/amd64 + linux/arm64).

## Quickstart 1 — solo, local registry (no network)

Bring up a local Zot + CNCF Distribution pair via `make dev-up`, then create a
bucket against each. Zero credentials, zero network traffic.

```bash
git clone --recursive https://github.com/meteora-pro/lofs && cd lofs
make dev-up          # Zot on :5100, Distribution on :5101 (docker-compose)

# defaults to Zot at http://localhost:5100
lofs doctor
lofs create demo --ttl-days 7
lofs list

# same binary, either registry:
lofs --registry http://localhost:5101 list

# full e2e suite + benchmarks
make test-e2e
make bench

make dev-down
```

Shared with your team but only on your dev box? Point a `LOFS_REGISTRY=http://<host>:5100` at a
Zot you already host and everyone uses it — no auth needed on a trusted LAN.

## Quickstart 2 — cross-machine handoff via GitLab.com

Share buckets across machines (e.g. macOS dev + Windows CI server) by pointing
LOFS at GitLab's public Container Registry.

### Step 1 — create a GitLab project + token

1. **New project** at https://gitlab.com/projects/new — name it `lofs-testbed` (or anything). Visibility: **Private**. Its container registry URL is `registry.gitlab.com/<your-username>/lofs-testbed`.

2. **Personal Access Token** at https://gitlab.com/-/user_settings/personal_access_tokens:
   - Scopes: `read_registry`, `write_registry` (for create/list/stat; push/pull flows)
   - For `lofs rm` to work, currently needs manual deletion via the GitLab UI / API — managed GitLab closes the public DELETE manifest endpoint (see [known limitations](#known-limitations)).
   - Copy the `glpat-…` token — it's shown only once.

### Step 2 — point LOFS at it

```bash
export LOFS_REGISTRY=https://registry.gitlab.com/<your-username>/lofs-testbed
export LOFS_REGISTRY_USERNAME=<your-gitlab-username>
export LOFS_REGISTRY_TOKEN=glpat-xxxxxxxxxxxxxxxxxxxx
```

### Step 3 — use it

```bash
lofs doctor
#   registry:   https://registry.gitlab.com/<user>/lofs-testbed
#   auth:       basic:<user>
#   prefix:     <user>/lofs-testbed
#   status:     ok (0 bucket(s) visible)

lofs create research-handoff --ttl-days 2
lofs create oauth-refactor --org meteora --ttl-days 5
lofs list
#   NAME              ORG      STATUS  TTL(d)  SIZE(MB)  EXPIRES           ID
#   oauth-refactor    meteora  active  5       1024      …
#   research-handoff  -        active  2       1024      …

# on your Windows box (same env vars set):
lofs list                       # sees the same buckets
lofs stat research-handoff      # reads them back identically
```

That's the primitive. Phase 1.2 (`lofs mount rw / unmount commit`) layers the actual file-based
handoff workflow on top — same registry, now with full POSIX overlay content, not just identity.

## Known limitations (v0.0.1)

- **`mount` / `unmount`** — not implemented yet; CLI prints a clean "not supported on this platform" error. These are Phase 1.2 (Linux FUSE backend + intent-manifest coordination, see [ADR-002](docs/architecture/adr/ADR-002-cooperative-coordination.md)).
- **`rm` against managed GitLab.com** — GitLab closes the OCI DELETE manifest endpoint for hosted registries; use the project's **Container Registry** UI to drop tags. Self-hosted GitLab with registry DELETE enabled, Zot, and Distribution all work fully. Native GitLab API fallback is tracked as Phase 1 follow-up.
- **No keyring integration** — credentials live in env vars / `~/.docker/config.json`. OS keychain support is on the roadmap.

See [`docker/docker-compose.yml`](docker/docker-compose.yml) for the
registry setup and [`bench/registry-comparison.md`](bench/registry-comparison.md)
for the latest Zot / Distribution behavioural matrix.

## Status

This repository is a concept scaffold. Design is in review. Not recommended for any real use yet. Implementation work is tracked via GitHub issues once the architecture is accepted.

If you're curious about the thinking that led here, read the ADRs and RESEARCH docs in [docs/architecture/](docs/architecture/).

## Documentation

- [ADR-001: LOFS design](docs/architecture/adr/ADR-001-lofs.md) — overall architecture + evolution roadmap (L0 → L7)
- [ADR-002: Cooperative Coordination](docs/architecture/adr/ADR-002-cooperative-coordination.md) — OCI-only intent-manifest model, no mandatory DB
- [IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) — phased delivery plan + library usage matrix + testing plan
- [Research directory](docs/architecture/research/) — six deep-dives on CRDT-FS space, layered storage, coordination, Rust ecosystem, prior art (historical — superseded by ADR-002 for coordination)
- [Contributing](CONTRIBUTING.md)

## License

Apache 2.0 — see [LICENSE](LICENSE).

## Part of Meteora

- **[LOKB](https://github.com/meteora-pro/lokb)** — Local Offline Knowledge Base (permanent memory tier)
- **[devboy-tools](https://github.com/meteora-pro/devboy-tools)** — DevBoy MCP server with plugin system
- **lofs** (this repo) — ephemeral shared workspace tier
