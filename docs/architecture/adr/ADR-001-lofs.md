---
id: ADR-001
title: LOFS — Layered Overlay File System для AI-агентов
status: proposed
date: 2026-04-22
deciders: ["Andrey Maznyak"]
tags: ["rust", "storage", "mcp", "agents", "oci", "rootless", "overlay", "fork-merge", "multi-agent-coordination"]
supersedes: null
superseded_by: null
related_goals: []
related_issues: []
---

# ADR-001: LOFS — Layered Overlay File System для AI-агентов

## Status

**proposed** — четвёртая итерация дизайна, фиксирующая финальный нейминг и L0 minimum. Итерационная история:

- **v1** CRDT-based shared workspace (3-layer Loro + HLC + transactional shell) — отвергнут как research-territory, см. [RESEARCH-002](../research/RESEARCH-002-crdt-fs-space.md).
- **v2** pivot к fork-merge модели, LLM-driven merge — первый production-ready подход.
- **v3** углубление через [RESEARCH-003](../research/RESEARCH-003-layered-bucket-storage.md) + [RESEARCH-004](../research/RESEARCH-004-rustfs-oci-coordination.md) — CDC + packs, OCI artifacts, MAST-based coordination, HITL hooks.
- **v3.5** pivot к **mount/unmount L0** — 4 MCP тула поверх OCI layers + rootless overlay (Buildah/libfuse-fs ecosystem). [RESEARCH-005](../research/RESEARCH-005-rust-oci-ecosystem.md) зафиксировал Rust-стек.
- **v4 (текущий)** — finальный нейминг **LOFS**, OSS repo `meteora-pro/lofs`, pair с LOKB. Competitive landscape — [RESEARCH-006](../research/RESEARCH-006-oss-prior-art.md).

## Context

### Реальные use-cases агентов

1. **Handoff между агентами.** Агент A провёл research (analysis.md, скриншоты, логи, CSV) → агент B получает ссылку и пишет MR.
2. **Fan-out / fan-in.** Оркестратор разбивает задачу на N sub-task'ов, sub-agent'ы в своих sandbox'ах, оркестратор собирает.
3. **Checkpointing долгих задач.** Часовая работа — periodic commit'ы; runtime упал — следующий instance с последнего snapshot'а.
4. **Collective accumulation.** 3 агента параллельно накапливают notes (каждый в своём fork'е, потом merge).
5. **Big-data handoff.** Бот проанализировал 100 GB логов → передаёт downstream-обработчику.

### Общие свойства

- **Короткий lifetime** — часы/дни/недели (не годы).
- **Смешанное содержимое** — текст + бинарники + дампы.
- **Handoff / fan-out** (не realtime collab).
- **Агент — активный участник merge** (intelligence in loop).
- **Ссылочная композиция** как git submodule, но без боли.

### Что НЕ делаем

- **Не заменяем git** — для основной кодовой базы git остаётся, мы дополняем там где он неудобен.
- **Не строим realtime shared editor.**
- **Не делаем distributed POSIX-FS** — это object-store с git-like снимками.

### Ограничения

1. Agent sandbox обычно без `CAP_SYS_ADMIN` → FUSE опционален.
2. S3-like eventual consistency cross-region → bias к immutable content-addressable.
3. LLM-merge стохастичен → safeguards (quarantine, dry-run, review-before-execute).
4. Storage растёт → TTL + ref-counting GC с day 0.
5. DevBoy инфра (OIDC, Postgres, Redis) — переиспользуется.

## Decision

> **Решение:** построить `lofs` — agent-scoped ephemeral workspace с git-like `snapshot / fork / merge / sub-mount` семантикой, chunked blob-pool (FastCDC + pack-files), hot tier через self-hosted S3-compatible (MinIO primary, RustFS когда GA), S3 cold mirror + Glacier archive. Merge — review-centric proposal с 4-tier strategy (trivial → syntax-aware → LLM-driven → human). Sharing через OCI artifacts (Zot/Harbor). Multi-agent coordination — explicit intent metadata + 7-state claim lifecycle + HITL hooks. Никакого CRDT на hot-path.

### Объекты

| Объект | Что это | Mutability |
|--------|---------|-----------|
| **Bucket** | Named workspace с TTL (дефолт 30d) | mutable head |
| **Snapshot** | Immutable версия bucket'а (~ git commit) | immutable, content-addressable |
| **Tree** | Канонически-сериализованная `path → chunk_ref[]` map | immutable |
| **Chunk** | Content-defined chunk (BLAKE3-hashed, FastCDC boundaries) | immutable, globally dedup |
| **Pack** | Bundle из N chunks, zstd:chunked compressed, ~16 MiB target | immutable |
| **Blob (logical)** | File view — sequence of chunk_refs | immutable |
| **Fork** | Новый bucket от чужого snapshot'а (COW через shared CAS) | new bucket |
| **Intent** | Декларация цели fork'а (goal, scope, labels, TTL) | lifecycle-managed |
| **MergePlan** | Three-way diff с auto-resolutions + reasoning | transient, reviewable |
| **SubMount** | OCI subject-ref от snapshot'а parent bucket'а к snapshot'у другого | часть tree |

### Architecture

```
┌───────────────────────────────────────────────────────────────────────────────┐
│                           Agent runtime                                        │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │  bucket.*    │  │  snap.*      │  │  merge.*     │  │  intent.*    │      │
│  │  fork.*      │  │  ws.*        │  │  review.*    │  │  publish.*   │      │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘      │
└─────────┼────────────────┼─────────────────┼─────────────────┼────────────────┘
          │                │                 │                 │
  ┌───────▼────────────────▼─────────────────▼─────────────────▼────────────┐
  │                   lofs core (Rust)                            │
  │  ┌──────────────────────────────────────────────────────────────────┐   │
  │  │  Snapshot DAG + Tree canonical-encode (CBOR + BLAKE3)            │   │
  │  ├──────────────────────────────────────────────────────────────────┤   │
  │  │  Chunking (FastCDC) + Pack writer (zstd:chunked) + Pack index    │   │
  │  ├──────────────────────────────────────────────────────────────────┤   │
  │  │  Intent registry (Postgres canonical + Redis active)              │   │
  │  ├──────────────────────────────────────────────────────────────────┤   │
  │  │  MergeStrategy ladder (Trivial → TreeSitter → LLM → Human)       │   │
  │  ├──────────────────────────────────────────────────────────────────┤   │
  │  │  Cold-tier summarizer (AI summary, Ed25519-signed, embeddings)    │   │
  │  └────────────┬─────────────────┬──────────────────┬─────────────────┘   │
  │               │                 │                  │                      │
  │      ┌────────▼──────┐   ┌──────▼────────┐  ┌─────▼──────────┐          │
  │      │  OpenDAL hot  │   │  OpenDAL cold │  │ OCI client     │          │
  │      │  (S3 compat)  │   │  (S3 mirror)  │  │ (publish/share)│          │
  │      └────────┬──────┘   └──────┬────────┘  └─────┬──────────┘          │
  └───────────────┼─────────────────┼──────────────────┼──────────────────────┘
                  │                 │                  │
         ┌────────▼──────┐  ┌───────▼─────────┐  ┌─────▼────────────┐
         │ MinIO/RustFS/ │  │  AWS S3 (IA /   │  │  Zot / Harbor    │
         │  SeaweedFS    │  │   Glacier)      │  │  OCI registry    │
         │  (hot)        │  │  (cold mirror)  │  │  (shareable)     │
         └───────────────┘  └─────────────────┘  └──────────────────┘
```

### MCP tools API

```
# Bucket lifecycle
bucket.create(name, ttl_days?, labels?) → bucket_id
bucket.stat(bucket_id) → metadata + active intents + warnings
bucket.list(org_id, filter?)
bucket.extend_ttl(bucket_id, days)
bucket.promote_persistent(bucket_id)
bucket.delete(bucket_id)                # immediate or on TTL

# Snapshots
snap.write(bucket, path, bytes)         # → pending-writes
snap.delete(bucket, path)               # tombstone
snap.status(bucket) → pending_diff
snap.commit(bucket, message?, expected_parent?) → snapshot_id
snap.list(bucket), snap.get(snapshot_id)
snap.bookmark(snapshot_id, label?, ttl?)
snap.unbookmark(snapshot_id)
snap.list_bookmarks(bucket)

# Reads
ws.read(bucket, path, snapshot_id?) → bytes
ws.ls(bucket, prefix?, snapshot_id?)
ws.blame(bucket, path) → history
ws.stat(bucket, path) → metadata

# Fork
fork.create(source_snapshot, intent: IntentSpec, new_name?) → new_bucket
fork.parent(bucket)
fork.rebase(bucket, new_base)
fork.drift(bucket) → count + warning_level

# Merge (review-centric)
merge.propose(source_snap, target_snap, base?) → MergePlan
merge.auto_resolve(plan) → partial_plan       # Tier 1+2
merge.review(plan) → ReviewReport             # per-conflict reasoning
merge.suggest(plan, conflict_id) → Resolution # LLM per-conflict
merge.override(plan, conflict_id, resolution) # agent OR human edit
merge.audit_decisions(plan) → decision_log
merge.dry_run(plan, resolutions) → SnapshotPreview
merge.execute(plan, approved_resolutions) → snapshot_id
merge.approve(merge_id, decision)             # HITL gate
merge.recover(quarantine_ref) → blob_ref

# Intent lifecycle (coordination)
intent.declare(bucket, IntentSpec) → intent_id
intent.start(intent_id)
intent.heartbeat(intent_id)             # every 30-60s, TTL 5min
intent.update(intent_id, {status, progress, blocker})
intent.complete(intent_id, outcome)
intent.abandon(intent_id, reason)
intent.handoff(intent_id, to_agent, context_artifact)
intent.discover(org, filter)            # with scope-overlap, active-since
intent.who_is_touching(bucket, path?)
intent.negotiate(other_agent, proposal) → decision  # max 2 rounds
intent.escalate_human(intent_id, reason)
intent.verify_outcome(intent_id)        # dual-LLM drift check

# Composition (OCI subject-refs)
submount.add(parent_bucket, path, target_snapshot)
submount.update(parent_bucket, path, new_snapshot)
submount.resolve(bucket, path) → materialized_tree

# Publish / share (via OCI artifacts)
publish.freeze(snapshot_id, registry_ref, ttl?) → oci_artifact_ref
publish.import(oci_artifact_ref) → new_bucket_as_fork
publish.list_shared(org) → [artifact_ref]

# History & audit
history.narrative(bucket, range)        # AI-generated summary
history.by_agent(bucket, agent_id)
history.by_path(bucket, glob, range)
history.semantic_search(bucket, query)  # embedding search
history.retrieve_archive(snap_id)       # best-effort cold recovery
```

### Storage layer (hot + cold)

**Hot tier** — self-hosted S3-compatible через OpenDAL. Выбор backend'а plural:

| Backend | Maturity | License | Use |
|---------|----------|---------|-----|
| **MinIO** | production, battle-tested | AGPL-3.0 / commercial | **Primary default** |
| **SeaweedFS** | production | Apache 2.0 | Dark-horse для pack-heavy workloads |
| **RustFS** | ⚠️ alpha (distributed НЕ GA в 2026 H1) | Apache 2.0 | Switch when GA (~2026 H2) |
| **Garage** | production | AGPL-3.0 | Geo-distributed small clusters |

Swap backend — прозрачный через OpenDAL. **Start with MinIO, spike #5 чтобы выбрать финально.**

**Cold tier** — AWS S3 / GCS / R2 с lifecycle-policy tier-transitions:

```
Hot (MinIO/RustFS)  ──native replication──▶  S3 Standard-IA
                                                    │ 30d
                                                    ▼
                                            S3 Glacier Instant Retrieval
                                                    │ 180d
                                                    ▼
                                            S3 Glacier Deep Archive
```

**Read-through fallback:** hot 404 → our lofs-daemon fetches from cold → repopulates hot (write-back cache).

### Chunking + Pack-files

**CDC:** [`fastcdc`](https://crates.io/crates/fastcdc) v3.x (v2020 algorithm), ~2 GB/s single-core.

**Chunking profiles (auto-selected по MIME):**

```toml
[chunking.mixed]    # default
min = "16 KiB"
avg = "256 KiB"
max = "1 MiB"

[chunking.code]     # text/code/config files
min = "2 KiB"
avg = "8 KiB"
max = "32 KiB"

[chunking.binary]   # large/opaque binaries
min = "64 KiB"
avg = "1 MiB"
max = "4 MiB"
```

**Pack-files** — Borg-style segments:
- **Target 16 MiB** — align с RustFS 1 MiB Reed-Solomon block (16× RS blocks) и >S3 multipart minimum (5 MiB).
- Format: `EncryptedBlob1 || ... || EncryptedBlobN || EncryptedHeader || Header_Length`.
- Compression: **zstd-3 hot**, **zstd-19 archive**.
- **zstd:chunked format** внутри — позволяет Range-GET без decompressing всего pack'а (SOCI-style lazy pull).
- **Pack index** → Postgres + `moka` LRU + Bloom filter per-bucket.
- **Pack-reorganize** (триггеры: pack > 100 MB split, pack < 4 MB старше 7d merge).

### Wire formats (OCI-compatible media types)

Snapshot и связанные artifacts сериализуются как **OCI artifacts** — это даёт interop с Zot/Harbor/CNCF Distribution, cosign signing, Referrers API для композиции.

**Media types (vendor-prefix `vnd.devboy.*`):**

```
application/vnd.meteora.lofs.snapshot.v1+json     # root manifest
application/vnd.meteora.lofs.tree.v1+cbor         # tree blob
application/vnd.meteora.lofs.pack.v1.zst          # pack layer (zstd:chunked)
application/vnd.meteora.lofs.packindex.v1+json    # zTOC для pack
application/vnd.meteora.lofs.mergeplan.v1+json    # merge plan (subject-ref'd)
application/vnd.meteora.lofs.auditlog.v1+parquet  # audit log segment
application/vnd.meteora.lofs.summary.v1+json      # cold-tier AI summary
```

IANA registration — **не требуется для MVP** (Helm ждал 4 года; WASM не регистрировали). Vendor-prefix достаточен.

### Merge system — review-centric

Merge — **не финальное действие**, а **reviewable proposal**. Agent (или human) всегда может override.

**Flow:**

```
merge.propose(src, tgt, base?)
      ↓
   MergePlan {
     auto_accepted,   // трivial + side-only
     conflicts: [{
        path, category, severity,
        auto_suggestion?,       // pre-computed
        reasoning,              // объяснение алгоритма
        base/ours/theirs blobs,
     }],
     summary, statistics
   }
      ↓
   merge.auto_resolve(plan)    // Tier 1+2 применяются auto
      ↓
   merge.review(plan)          // ReviewReport: risks, semantic checks, LLM summary
      ↓
   merge.suggest(plan, id)     // per-conflict LLM proposal (Tier 3)
      ↓
   merge.override(plan, id, resolution)   // agent OR human корректирует
      ↓
   merge.dry_run(plan, resolutions) → preview
      ↓
   merge.execute → new snapshot
```

**4-tier MergeStrategy ladder:**

```rust
enum MergeStrategy {
    // Tier 1: cheap, exact
    Identical,
    SideOnly,
    LwwByTimestamp,

    // Tier 2: syntax-aware (cheap, deterministic)
    TreeSitterMergiraf,    // 33 языков, prod-ready
    TreeSitterWeave,       // entity-level, early-adopter
    StructuredYq,          // JSON/YAML/TOML simple cases

    // Tier 3: semantic (expensive, LLM-backed)
    AgentDriven {
        model: String,
        dual_verify: bool,     // cross-check via second LLM
        tools: Vec<Tool>,      // read_file, run_test, blame, diff
    },

    // Tier 4: escalate
    HumanReview,               // quarantine + notify
}
```

**Per-conflict flow**: try Tier 1 → fall to Tier 2 → to Tier 3 → to Tier 4.

### Merge-agent toolset (8 tools)

Агент-resolver (или human reviewer) получает:

1. `ws.read(path, snapshot_id)` → bytes
2. `ws.blame(path, snapshot_range)` → change history
3. `merge.threeway_diff(path)` → structured hunks
4. `merge.related_changes(entity_name)` → related files via AST-index
5. `merge.run_check(path)` → `{syntax_ok, type_ok, test_result}` через LSP + test-runner
6. `merge.ask_peer_agent(source_snapshot, question)` → text (если source от другого agent'а)
7. `merge.quarantine(content)` → blob_ref (save loser, never silently lose)
8. `merge.dry_run(proposed_merge)` → `{conflicts_count, warnings}`

### Binary-no-diff policy

- **Binary files** (detected по MIME/extension): merge = **LWW + quarantine-both**, **НЕ** byte-diff. Hash + size + mime в metadata только.
- **Text large:** soft-limit 500 KB — агент пишет **код** для filter/analyze через `bash`/`run_script` tools, не читает raw.
- **Text very large:** hard-limit 5 MB — require split-merge или human review.

```toml
[merge.diff]
binary_mode = "metadata_only"      # hash + size + mime, никаких byte-diffs
text_diff_soft_limit = "500 KiB"
text_diff_hard_limit = "5 MiB"
```

### Intent & claim lifecycle

**States:**

```
pending → claimed → working → [blocked | awaiting_input | awaiting_approval]
                            → completed | failed | canceled | abandoned
```

- **Interrupt states** (input/auth required) — temporary pause, resumable.
- **Terminal states** — permanent.

**Heartbeat:** every **30-60s**, TTL **5 min**. Stale claim → auto-release, fork остаётся с `abandoned_at` в journal. Другой agent может `fork.rebase` и continue.

**IntentSpec schema:**

```rust
pub struct IntentSpec {
    task_id: TaskId,
    goal: String,                           // 1-3 sentence NL
    scope: Vec<PathGlob>,                   // explicit bounds
    estimated_duration: Duration,
    declared_by: AgentId,
    declared_at: Timestamp,
    blocks_on: Vec<TaskRef>,
    labels: BTreeMap<String, String>,       // tracker_id, ticket, priority
    success_criteria: Vec<String>,
    constraints: Vec<String>,               // "do not modify tests/"
    parent_intent: Option<TaskRef>,
}
```

Schema compatible с [A2A TaskCard](https://a2a-protocol.org/latest/specification/) (future interop).

**Coordination rules:**
- **Max handoff hops = 3**, потом timeout-to-human.
- **Max negotiation rounds = 2**, потом force human-arbitration или FCFS.
- **Discovery ergonomics:** warnings integrated в `bucket.stat` tool description (LLM читает в prompt).
- **Structured topology:** orchestrator + ≤20 workers per bucket; > 20 → split в sibling buckets via sub-mount. "Bag of agents" (> 4 без topology) — **explicit antipattern** (17× error trap).
- **Intent drift detection:** dual-LLM verify (second reads goal + commits → similarity score); server-side path-glob enforcement на commit.

**MAST-based test matrix** для Phase 7 loadtest: 100 agents × 10 buckets, measure frequency of each of 14 failure modes.

### Human-in-the-loop (HITL) hooks

Per-bucket policy в `.lofs.toml`:

```toml
[approval]
sensitive_paths = ["apps/**/migrations/**", "secrets/**", ".werf/**"]
on_merge = "require_human"       # для sensitive
on_commit = "auto"
on_delete = "require_human"
on_publish = "audit_notify"      # email к org admins
```

**Hook points:**
- `merge.execute(plan)` проверяет sensitive_paths → interrupt → wait `merge.approve(merge_id, human_id, decision)`.
- `bucket.delete` на non-ephemeral → require confirmation.
- `publish.freeze` → optional audit email.
- Integration с DevBoy UI: dashboard pending-approval queue.

Pattern заимствован у LangGraph interrupt + GitHub Copilot Workspace ("agent produces artifact, human approves, не auto-merge").

### Tiered retention & AI-generated summaries

Данные существуют одновременно в трёх representations'ах:

1. **Hot (≤ 7 days, ≤ 20 snapshots).** Full detail, все chunks доступны в hot storage.
2. **Warm (7-60 days).** Consolidated snapshot per week, content в cold-tier S3-IA. Chunks reconstructable через lazy-pull.
3. **Cold (> 60 days).** **Только AI-generated summary** (narrative + metadata + dropped_blob hashes). Raw data в Glacier Deep Archive (best-effort 12h retrieve).

**Compaction triggers:**

| Trigger | Default | Action |
|---------|---------|--------|
| Time-based | > 30d | warm consolidation |
| Count-based | > 200 snapshots | week-packs |
| Size-based | bucket > 10 GB | force tier-check |
| Explicit | `bucket.compact(until)` | manual |
| Bookmark | `held_by != null` | skip from consolidation |

**AI-summary schema (v1):**

```json
{
  "schema_version": "v1",
  "period": {"start_snapshot", "end_snapshot", "start_ts", "end_ts", "snapshots_consolidated"},
  "agents": [{"agent_id", "snapshot_count"}],
  "changed_paths": [{"glob", "change_kind", "lines_delta"}],
  "narrative": {"summary_short", "summary_full", "key_decisions"},
  "bookmarked_snapshots": [{"snapshot_id", "reason"}],
  "omitted_blobs": {"count", "total_size_bytes", "archive_pointer", "archive_manifest"},
  "signatures": {"summary_hash_blake3", "prev_summary_hash_blake3", "signed_by", "signature"},
  "embedding": {"model", "vector_ref"}
}
```

- Ed25519-signed для tamper-evidence.
- Prev-summary-hash chain → Merkle integrity.
- Embedding в Qdrant / pgvector для `history.semantic_search`.

**Cost implications** (10 TB hot + 10 TB cold mirror):
- All S3 Standard (naive): ~$2760/year.
- Tiered (hot self-hosted + S3 cold): **~$100-250/mo ≈ $1200-3000/year** (но hot self-hosted → compute cost добавляется, итого comparable; decoupled от read volume).
- Savings from dedup CDC + compaction summary: **2-5× storage**, **read bandwidth amortized**.

### Bookmark mechanism

```
snap.bookmark(snapshot_id, label?, ttl?)
snap.unbookmark(snapshot_id)
snap.list_bookmarks(bucket)
```

ZFS-hold pattern: `bookmark: {label, held_by, ttl}` в snapshot metadata. Excluded from all compaction. Auto-bookmark triggers: `publish.freeze`, `merge.to_main`, `agent.explicit`.

### Security & ACL

- **OIDC claims → bucket ACL.** Scopes: `read / write / fork / merge / admin / publish` — per-bucket.
- **Presigned URLs** для direct blob-reads (bypass API для больших файлов).
- **Ed25519 op-signing (opt-in)** — каждый commit/merge/intent-lifecycle op подписан agent-ключом. Rolling rotation 24h, validity window 72h.
- **Cosign для shareable snapshots** (publish.freeze → cosign sign). Keyless через Fulcio + OIDC + Rekor transparency log. Internal Ed25519 — для hot-path, cosign — для external shares.
- **Share tokens** — short-lived JWT (24h default) с claims `{snapshot_id, permissions, issued_by}` или OCI artifact ref.
- **SubMount** респектит ACL target-bucket'а.

### Что намеренно не входит

Упрощения против предыдущих итераций:

- **Нет CRDT** на hot-path (tree, text, move-op). Snapshot-DAG + explicit merge покрывают.
- **Нет HLC.** Snapshot parent-links дают causal ordering.
- **Нет transactional shell через btrfs snapshot.** Агент работает на fork'е (отдельный bucket).
- **Нет viewID для concurrent writes.** Optimistic lock через `expected_parent_snapshot` на commit.
- **Нет continuous sync daemon.** Forks статичны до явного merge.
- **Нет move-tree CRDT, Loro, Automerge, uhlc, tombstone-GC, redb-для-op-log.** Всё это было под CRDT-модель.

Это снимает ~60% complexity budget'а от CRDT-варианта.

### Engineering stack

| Компонент | Выбор | Fallback |
|-----------|-------|----------|
| Hot storage | **MinIO** (primary) через OpenDAL | SeaweedFS, RustFS когда GA |
| Cold storage | **AWS S3** (IA → Glacier) через OpenDAL | GCS, R2 |
| Storage abstraction | **OpenDAL** (pin conservative) | — |
| Content hashing | **BLAKE3** | SHA-256 |
| Chunking | **`fastcdc` v3.x** (v2020 algorithm) | SeqCDC when > 1 GB/s bottleneck |
| Snapshot hashing | **BLAKE3** поверх canonical CBOR | — |
| Tree canonical | **`ciborium`** (deterministic CBOR) | MessagePack |
| Compression (hot pack) | **zstd-3** | — |
| Compression (archive) | **zstd-19** | — |
| Pack format | **zstd:chunked** (Podman GA 2026) | Plain zstd + SOCI-style zTOC |
| OCI artifact client | **`oci-client`** + **`oci-spec`** | `oras` crate |
| OCI registry primary | **Zot** (self-hosted, Apache 2.0) | Harbor, CNCF Distribution |
| Metadata KV | **PostgreSQL** (DevBoy) | — |
| Active state / heartbeats | **Redis** (DevBoy) | — |
| Chunk-index cache | **`moka`** (LRU/TinyLFU) | — |
| Audit query | **DuckDB over S3 parquet** | ClickHouse |
| Embedding store | **Qdrant** or **pgvector** | FAISS (no filter) |
| Structured merge | **Mergiraf** (tree-sitter, 33 langs) via subprocess | Weave (early-adopter) |
| Op signing | **`ed25519-dalek`** | — |
| External artifact signing | **`cosign`** (sigstore) | — |
| FUSE (opt-in) | **`fuse3_opendal` / `ofs`** | fuser + custom |
| MCP protocol | **`rmcp`** (official Rust SDK) | — |

### Storage layout

```
<hot-store>/
├── org/<org_id>/
│   ├── blobs/packs/<pack_hash>.zst        ← pack files (zstd:chunked)
│   ├── blobs/index/<pack_hash>.ztoc       ← Range-GET lookup
│   ├── buckets/<bucket_id>/
│   │   ├── meta.json
│   │   ├── head                           ← current snapshot_id
│   │   └── refs/<label>                   ← named refs
│   ├── snapshots/<snapshot_id>.json       ← OCI manifest
│   ├── merges/<merge_id>.json             ← plan + decisions audit
│   └── audit/<yyyy-mm-dd>/<bucket_id>.jsonl
│
<cold-store>/                              ← mirror of hot + archive tier
├── ...same structure...
└── archive/<period>/period.tar.zst         ← cold-tier dropped blobs
```

`snapshot_id` = BLAKE3(canonical_serialize(snapshot)) включая parent, tree, metadata — Merkle-DAG проверяемость.

Blob-pool **один на org** (не per-bucket). Fork от 50 GB bucket'а = килобайты metadata, не копирование данных.

## Consequences

### Positive

- ✅ **Mental model git-like** — понятна любому инженеру.
- ✅ **Complexity ядра** ниже чем у CRDT-модели на ~60%.
- ✅ **LLM как merge-driver** — уникальный differentiator.
- ✅ **Review-centric flow** — agent или human всегда может override, нет "чёрного ящика".
- ✅ **Binary + large-file native** — CAS не различает, pack-files дёшевы.
- ✅ **Chunk-level dedup** через FastCDC — 2-5× storage savings vs naive per-file blobs.
- ✅ **Hot/cold tiering** — **savings 10-25× vs all-S3-Standard** на realistic workload (10 TB).
- ✅ **OCI-compatible artifacts** — interop с Zot/Harbor, cosign signing, Referrers API free.
- ✅ **Multi-agent coordination first-class** — intent lifecycle, MAST-tested, deadlock mitigations.
- ✅ **HITL hooks** — sensitive paths не auto-merge.
- ✅ **Cost-effective** — TTL + ref-counting GC + cold summaries.
- ✅ **Нет research-level risk.** Merkle-DAG + CAS + OCI — 20+ лет проверенных паттернов.
- ✅ **Cross-cloud** — OpenDAL ~40 backend'ов бесплатно.

### Negative

- ❌ **Нет realtime collaboration** (два агента live в одном файле → fork required).
- ❌ **Merge conflicts реально случаются** (в CRDT по определению — нет).
- ❌ **LLM-merge стохастичен и платный** — tokens cost.
- ❌ **Stale forks** требуют monitoring + `fork.rebase` discipline.
- ❌ **Operational burden от pack-tuning** — Phase 7 experimental iteration.
- ❌ **Dependency на external registry** (Zot/Harbor) для publish.
- ❌ **Compaction теряет intermediate state** — trade-off storage vs full-trace.

### Risks (ранжировано)

- ⚠️ **#1 LLM-merge / summary hallucination** → тихая потеря context'а. *Митигация:* dual-model verify + quarantine обязательна + review-before-execute + `merge.dry_run` + Ed25519-signed summaries + archive-pointer recovery + human-approval для sensitive paths.
- ⚠️ **#2 ADS (Agent Deadlock Syndrome)** — 2+ agents защекиваются в circular handoff. *Митигация:* max-hop 3, max 2 rounds negotiation, timeout-to-human 5min, arbiter FCFS fallback.
- ⚠️ **#3 Blob-pool rampage** — orphan blobs накапливаются. *Митигация:* TTL 30d + ref-counting GC weekly sweep + dashboard top-N + alert quota.
- ⚠️ **#4 Fork drift** — merge-plan overflows LLM context. *Митигация:* hard-stop > 200, incremental rebase, split-plan per-subtree, auto-suggest rebase при > 50.
- ⚠️ **#5 Intent drift** — agent commits outside declared scope. *Митигация:* server-side path-glob enforcement, dual-LLM outcome verify, rejection на commit.
- ⚠️ **#6 RustFS alpha-state** — не для prod сейчас. *Митигация:* MinIO primary, swap через OpenDAL когда RustFS GA.
- ⚠️ **#7 Mergiraf GPL-3.0** vs наш Apache 2.0. *Митигация:* subprocess (exec-boundary), не статик-линк.
- ⚠️ **#8 Snapshot hash collision BLAKE3** — astronomically unlikely. *Митигация:* monitoring, author+counter в canonical form.
- ⚠️ **#9 S3 eventual consistency** cross-region. *Митигация:* conditional GET (`If-Match`), retry + exponential backoff.
- ⚠️ **#10 Pack tuning требует operational expertise** — нет universal answer. *Митигация:* Phase 7 dedicated tuning spike, metrics с day 0.
- ⚠️ **#11 SubMount cycles.** *Митигация:* DAG-валидация при `submount.add`, max-depth 16.
- ⚠️ **#12 Commit race.** Two agents concurrent commit. *Митигация:* optimistic через `expected_parent_snapshot`, loser получает `ParentMismatch` → `fork.rebase`.
- ⚠️ **#13 Adversarial / scope violation.** *Митигация:* Ed25519 signed ops, rate-limiting per-agent, `.lofs.toml` scope policies, server-side enforcement.
- ⚠️ **#14 Path-injection.** `../etc/passwd` в commit. *Митигация:* canonicalization + no-`..` / no-absolute validation.

## Alternatives Considered

### Alternative 1: CRDT-backed shared workspace (REJECTED в v2)
См. [RESEARCH-002](../research/RESEARCH-002-crdt-fs-space.md). Rejected: complexity не оправдан use-case'ами (handoff/fan-out, не continuous collab); research-level risk (CRDT-FS — research area, Loro/Automerge не production-ready); semantic breakage by-design (char-level CRDT → invalid JSON merges); LLM-as-merge-driver невозможен на char-level CRDT.

Reused primitives: BLAKE3 CAS, OpenDAL, Mergiraf strategy, Ed25519, audit/DuckDB.

### Alternative 2: Чистый git + git-LFS
Rejected: git нет TTL; LFS требует отдельный сервер + toolchain сложности; submodule — главная слабость; git optimize'н под human workflow; long-lived history — обуза.

### Alternative 3: AWS S3 Files (GA April 2026)
Rejected (но прямой конкурент): AWS-only; нет fork/merge; concurrent writes = LWW без quarantine; нет audit/LLM-merge/SubMount. Differentiator: cross-provider + ephemeral lifecycle + LLM-driven merge + OCI interop.

### Alternative 4: Fast.io / Daytona Volumes
Rejected: vendor lock-in; CRUD API без fork/merge; нет agent-driven merge как концепт; не переиспользуют DevBoy инфру.

### Alternative 5: Jujutsu (jj) as embedded library
Rejected (но много позаимствовали): jj — VCS, не blob-store; API и on-disk формат не подходят для 100-GB артефактов; подвижная dependency. Заимствовали: operation-log, immutable snapshots, first-class conflicts.

### Alternative 6: Pure A2A Protocol как core
Rejected: A2A покрывает inter-agent messaging + task lifecycle, но не storage + merge semantics. **Заимствуем его TaskCard schema** для IntentSpec (future-interop); A2A — не замена storage-layer.

### Alternative 7: Keyhive/Beelay E2EE sync (Ink & Switch)
Rejected на MVP: research-level, нет production users. *Watch-list* для multi-tenant SaaS future.

## Implementation

### Phase 0 — Research & design pivot ✅ (2026-04-22)
- [RESEARCH-002](../research/RESEARCH-002-crdt-fs-space.md) CRDT-FS → pivot в fork-merge.
- [RESEARCH-003](../research/RESEARCH-003-layered-bucket-storage.md) CDC + OCI + merge + intent + compaction.
- [RESEARCH-004](../research/RESEARCH-004-rustfs-oci-coordination.md) RustFS+S3 + OCI implementation + MAST coordination.
- Spike v1 в `spikes/lofs/` — валидирован CAS + OpenDAL + property-test approach.

### Phase 1 — Core primitives + storage layer (3 недели)
- Rust crate `lofs-core`:
  - BlobStore через OpenDAL (hot MinIO + cold S3).
  - FastCDC chunker (v2020) + content-type profiles.
  - Pack writer (zstd:chunked, 16 MiB target) + zTOC.
  - Tree (canonical CBOR), Snapshot (Merkle-DAG).
  - Bucket (head + TTL + ACL).
- Property tests: determinism, immutability, chunk-level dedup.

### Phase 2 — MCP tools + OCI wire format (3 недели)
- Rust crate `lofs-mcp` в `devboy-tools` plugin system.
- Tools: `bucket.*`, `snap.*`, `ws.read/ls/blame`, `fork.*`.
- OCI artifact serialization (`oci-client` + `oci-spec`).
- Custom media types + Referrers API support.
- Integration DevBoy OIDC + ACL + Redis pub/sub.

### Phase 3 — Merge engine + review tools (3 недели)
- `MergeStrategy` trait + 4-tier ladder.
- Mergiraf subprocess integration (exec-boundary для GPL).
- Weave integration (feature flag, early-adopter).
- `merge.propose/auto_resolve/review/suggest/override/audit_decisions/dry_run/execute/recover`.
- 8-tool merge-agent toolset (formal schema).
- Quarantine механика.
- Binary-no-diff policy enforcement.

### Phase 4 — Intent lifecycle + HITL + SubMount (3 недели)
- Intent state machine (7 states) + heartbeat (Postgres + Redis).
- 10 intent.* MCP tools.
- Max-hop enforcement, negotiation max 2 rounds, dual-LLM drift-detection.
- SubMount через OCI subject-references (Referrers API).
- HITL hooks: `.lofs.toml` approval policy, interrupt на merge/delete/publish.
- DevBoy UI integration (pending-approval queue).

### Phase 5 — Audit, publish, share (2 недели)
- Audit log в S3 parquet + DuckDB query endpoint.
- `publish.freeze` → OCI artifact push (Zot target).
- `publish.import` → OCI artifact pull.
- cosign sign/verify integration.
- `ws.blame` через audit query.

### Phase 6 — FUSE adapter (opt-in, 1-2 недели)
- `ofs`-based read-only mount для convenience.
- Write-through MCP API (не прямая FS-запись).
- Lazy materialization via tree → pack Range-GET.

### Phase 7 — Cold-tier compaction + summarizer (2 недели)
- Tiered retention (hot/warm/cold) implementation.
- Bookmarks с TTL protection.
- Cold-tier summarizer crate (`lofs-summarizer`).
- Ed25519-signed summaries + Merkle chain.
- Embedding generation + Qdrant index.
- `history.narrative/semantic_search/retrieve_archive`.

### Phase 8 — Hardening + observability (2 недели)
- Pack-reorganize (GC + repack).
- Reference-counting GC (weekly sweep).
- OpenTelemetry tracing per-tool.
- Prometheus metrics.
- **MAST-based loadtest** (100 agents × 10 buckets × 14 failure modes).
- Adversarial input fuzzing.
- Spike #5 (hot-tier choice) + Spike #6 (OCI roundtrip) + Spike #7 (MAST injection).

**Total realistic:** 16-19 недель с учётом всех подсистем и spikes. CRDT-вариант был бы ~24+ недель.

- **Задачи:** декомпозиция в ClickUp после ADR acceptance.
- **Код:** `external/devboy-tools/crates/lofs-*` (OSS Apache 2.0).

## References

### Research documents
- [RESEARCH-002: CRDT-FS space](../research/RESEARCH-002-crdt-fs-space.md) — исходная motivation для pivot.
- [RESEARCH-003: Layered bucket storage](../research/RESEARCH-003-layered-bucket-storage.md) — CDC + OCI + merge + intent + compaction.
- [RESEARCH-004: RustFS+S3 + OCI + coordination](../research/RESEARCH-004-rustfs-oci-coordination.md) — RustFS maturity + OCI implementation + MAST.

### Storage
- [FastCDC USENIX ATC'16](https://www.usenix.org/system/files/conference/atc16/atc16-paper-xia.pdf)
- [`fastcdc` Rust crate](https://crates.io/crates/fastcdc)
- [Restic design](https://restic.readthedocs.io/en/latest/design.html) · [Borg internals](https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html)
- [RustFS](https://rustfs.com/) · [RustFS GitHub](https://github.com/rustfs/rustfs) · [Self-hosted S3 2026 comparison](https://rilavek.com/resources/self-hosted-s3-compatible-object-storage-2026)
- [MinIO bucket replication](https://min.io/docs/minio/linux/administration/bucket-replication.html)
- [Apache OpenDAL](https://github.com/apache/opendal)
- [Sapling/EdenFS Inodes](https://github.com/facebook/sapling/blob/main/eden/fs/docs/Inodes.md) — lazy materialization.

### OCI
- [OCI Image Spec v1.1](https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md)
- [OCI 1.1 announcement](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/)
- [ORAS concepts + reftypes](https://oras.land/docs/concepts/reftypes/)
- [Helm OCI MediaTypes](https://helm.sh/blog/helm-oci-mediatypes/)
- [Zot registry](https://github.com/project-zot/zot) · [Harbor](https://goharbor.io/)
- [Rust `oci-client`](https://github.com/oras-project/rust-oci-client) · [`oci-spec-rs`](https://github.com/containers/oci-spec-rs)
- [SOCI snapshotter](https://github.com/awslabs/soci-snapshotter) · [Parallel Pull 2025](https://aws.amazon.com/blogs/containers/introducing-seekable-oci-parallel-pull-mode-for-amazon-eks/)
- [zstd:chunked Fedora 2026](https://discussion.fedoraproject.org/t/switch-fedora-container-images-to-support-zstd-chunked-format-by-default/123712)
- [Sigstore cosign](https://docs.sigstore.dev/cosign/signing/other_types/)

### Merge
- [Mergiraf](https://mergiraf.org/architecture.html) · [LWN Oct 2025](https://lwn.net/Articles/1042355/)
- [Weave (Ataraxy-Labs)](https://github.com/ataraxy-labs/weave)
- [LastMerge arXiv 2507.19687](https://arxiv.org/abs/2507.19687)
- [Kleppmann 2021 move-tree](https://martin.kleppmann.com/papers/move-op.pdf) (reference for alternative 1)
- [GitHub Copilot merge conflicts 2026](https://github.blog/changelog/2026-04-13-fix-merge-conflicts-in-three-clicks-with-copilot-cloud-agent/)
- [Greptile benchmarks 2025](https://www.greptile.com/benchmarks) · [State of AI Code Review 2025](https://www.devtoolsacademy.com/blog/state-of-ai-code-review-tools-2025/)

### Multi-agent coordination
- [MAST paper (arXiv 2503.13657)](https://arxiv.org/abs/2503.13657) — **test matrix**
- [A2A Protocol v0.3](https://a2a-protocol.org/latest/specification/)
- [Anthropic Multi-Agent Research System](https://www.anthropic.com/engineering/multi-agent-research-system)
- [Anthropic Harness design](https://www.anthropic.com/engineering/harness-design-long-running-apps)
- [LangGraph Supervisor](https://github.com/langchain-ai/langgraph-supervisor-py) · [HITL](https://docs.langchain.com/oss/python/deepagents/human-in-the-loop)
- [OpenAI Agents SDK handoffs](https://openai.github.io/openai-agents-python/handoffs/)
- [Temporal for AI](https://temporal.io/solutions/ai)
- [ADS — Agent Deadlock Syndrome](https://sanjana-nambiar.github.io/news29.html)
- [Towards Data Science — 17x Error Trap](https://towardsdatascience.com/why-your-multi-agent-system-is-failing-escaping-the-17x-error-trap-of-the-bag-of-agents/)
- [Mike Mason Jan 2026](https://mikemason.ca/writing/ai-coding-agents-jan-2026/)

### Compaction & audit
- [Kafka Log Compaction](https://docs.confluent.io/kafka/design/log_compaction.html)
- [Restic prune](https://restic.readthedocs.io/en/stable/060_forget.html) · [Borg prune](https://borgbackup.readthedocs.io/en/latest/usage/prune.html)
- [ZFS Holds](https://docs.oracle.com/cd/E19253-01/819-5461/gjdfk/index.html)
- [S3 Intelligent-Tiering](https://docs.aws.amazon.com/AmazonS3/latest/userguide/intelligent-tiering-overview.html)
- [MemGPT paper arXiv 2310.08560](https://arxiv.org/abs/2310.08560) — recursive summary pattern
- [APCE — AI Commit Explorer](https://arxiv.org/html/2507.16063v1)

### Meteora family
- [LOKB — Local Offline Knowledge Base](https://github.com/meteora-pro/lokb) — persistent memory tier (companion product)
- [devboy-tools](https://github.com/meteora-pro/devboy-tools) — DevBoy MCP server with plugin system (target consumer)

---

## Changelog

| Дата | Автор | Изменение |
|------|-------|-----------|
| 2026-04-22 | Andrey Maznyak | v1 draft — CRDT-based shared workspace (3-layer Loro + HLC + transactional shell). |
| 2026-04-22 | Andrey Maznyak | v1.1 — Phase 0 findings из [RESEARCH-002](../research/RESEARCH-002-crdt-fs-space.md): redb вместо Postgres op-log, LWW-HLC+Mergiraf default, viewID (ElmerFS), lazy materialization (EdenFS), Ed25519 op-signing, MergeEngine abstraction. |
| 2026-04-22 | Andrey Maznyak | **v2 major pivot** — fork-merge модель с LLM-driven merge-strategy. CRDT-модель ушла в Alternative 1 с явным обоснованием отказа. Phase-plan сократился с 20+ до 9-12 недель. |
| 2026-04-22 | Andrey Maznyak | **v3 — RESEARCH-003 integration**: CDC + pack-files (FastCDC v2020, Borg-style), chunk-content-type profiles, 4-tier MergeStrategy ladder, intent metadata + 10 MCP tools, bookmark mechanism, tiered retention (hot/warm/cold) с AI-summaries, cold-tier summarizer crate, compaction triggers. |
| 2026-04-22 | Andrey Maznyak | **v3.1 — User feedback integration**: review-centric merge (merge.override/audit_decisions обязательные), binary-no-diff policy (metadata only, никаких byte-diffs), text diff limits (soft 500KB, hard 5MB), hot tier = RustFS/S3 cold mirror explicit architecture. |
| 2026-04-22 | Andrey Maznyak | **v3.2 — RESEARCH-004 integration**: hot-tier plural (MinIO primary, RustFS когда GA, SeaweedFS alt) — RustFS alpha state blocker; formal OCI wire formats (`vnd.meteora.lofs.*`); publish/import через OCI artifacts (Zot primary); intent lifecycle formal 7-state + heartbeat 60s/TTL 5min + max-hop 3 + negotiation 2 rounds; HITL hooks first-class (`.lofs.toml` approval policy, interrupt на merge/delete/publish, GitHub Copilot Workspace pattern); SubMount через OCI subject-references; cosign integration для shareable snapshots; MAST taxonomy как Phase 8 test-matrix; pack size 16 MiB (aligned с RustFS 1 MiB RS blocks); zstd-3 hot / zstd-19 archive; chunk profiles (code/mixed/binary auto); ADS deadlock mitigation; 3 новых spike (#5 hot-tier choice, #6 OCI roundtrip, #7 MAST injection). Phase-plan 16-19 недель. |
| 2026-04-22 | Andrey Maznyak | **v3.5 — L0 pivot (mount/unmount)**: radical simplification. Вместо 40+ MCP tools — **4 тула L0**: `lofs.create / list / mount / unmount`. Агент получает обычный POSIX-путь (`/mnt/lofs/<session>`), работает любыми инструментами (cat, grep, cargo, git). Commit = OCI layer. Evolution L1-L7 только по data-driven триггерам. Backend: Buildah/libfuse-fs/ocirender для rootless overlay (RESEARCH-005). Весь "продвинутый" feature-set (CDC packs, intent lifecycle, tiered retention, HITL) отодвинут в L3+, per metrics. |
| 2026-04-22 | Andrey Maznyak | **v4 — LOFS naming + OSS repo**: renamed `bucket-teleport` / `CTXFS` → **LOFS (Layered Overlay File System)** после проверки availability (crates.io + lofs.dev free, минорные non-conflict). Pair с LOKB (Local Offline Knowledge Base) как Meteora family. Этот ADR перенесён в public repo `meteora-pro/lofs` как ADR-001. [RESEARCH-006](../research/RESEARCH-006-oss-prior-art.md) интегрирован: gap narrower чем initially казалось, differentiator = LLM-driven merge as first-class MCP primitive + OCI-artifact interop. Closest competitors: Cloudflare Project Think + Artifacts, ConTree (Nebius), Daytona. |
