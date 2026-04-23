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

**proposed** — пятая итерация дизайна. Итерационная история:

- **v1** CRDT-based shared workspace (3-layer Loro + HLC + transactional shell) — отвергнут как research-territory, см. [RESEARCH-002](../research/RESEARCH-002-crdt-fs-space.md).
- **v2** pivot к fork-merge модели, LLM-driven merge — первый production-ready подход.
- **v3** углубление через [RESEARCH-003](../research/RESEARCH-003-layered-bucket-storage.md) + [RESEARCH-004](../research/RESEARCH-004-rustfs-oci-coordination.md) — CDC + packs, OCI artifacts, MAST-based coordination, HITL hooks.
- **v3.5** pivot к **mount/unmount L0** — 4 MCP тула поверх OCI layers + rootless overlay (Buildah/libfuse-fs ecosystem). [RESEARCH-005](../research/RESEARCH-005-rust-oci-ecosystem.md) зафиксировал Rust-стек.
- **v4** — finальный нейминг **LOFS**, OSS repo `meteora-pro/lofs`, pair с LOKB. Competitive landscape — [RESEARCH-006](../research/RESEARCH-006-oss-prior-art.md).
- **v4.1 (текущий)** — координация вынесена в отдельный [ADR-002](ADR-002-cooperative-coordination.md): OCI-реестр стал единственным обязательным backend'ом, SQL/Redis — опциональные extension'ы. Разделены **L0 active scope** и **L1-L7 evolution roadmap**. CDC/pack/merge-engine/HITL больше не в Decision — они в "Future Evolution" и активируются data-driven триггерами.

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

1. Agent sandbox обычно без `CAP_SYS_ADMIN` → FUSE опционален (MVP: rootless через user namespaces).
2. Registry eventual consistency cross-region → bias к immutable content-addressable.
3. LLM-merge (L1+) стохастичен → safeguards (quarantine, dry-run, review-before-execute).
4. Storage растёт → TTL + ref-counting GC с day 0.
5. **Нет обязательной внешней БД.** OCI-реестр — единственный обязательный backend координации (см. [ADR-002](ADR-002-cooperative-coordination.md)). DevBoy инфра (OIDC, Postgres, Redis) переиспользуется **опционально** — как auth-provider (OIDC) и strong-lock backend (Redis/Postgres extension).

## Decision

> **Решение (L0 MVP):** построить `lofs` — agent-scoped ephemeral workspace с git-like snapshot-семантикой, где **OCI-реестр — единственный обязательный backend** и для контента (layers), и для координации (intent manifests через Referrers API). Агент получает обычный POSIX-путь через rootless FUSE overlay, работает стандартными инструментами (cat, grep, cargo, git). Каждый commit = новый OCI-слой. Координация между агентами **cooperative** (см. [ADR-002](ADR-002-cooperative-coordination.md)) — intent-декларации, pull-before-write, path-scoped writes. Никакой обязательной SQL/Redis-зависимости; Postgres/Redis/etcd подключаются как `Coordination` extension-backends для команд с requirement на strong guarantees.
>
> **L1-L7 evolution** — CDC + pack-files, LLM-driven merge-engine, 10-tool intent lifecycle, tiered retention, AI-summaries, HITL hooks — добавляется **только** когда telemetry показывает необходимость. Ни один из этих компонентов не предполагается в MVP.
>
> Никакого CRDT на hot-path (rejected в v2 — см. [RESEARCH-002](../research/RESEARCH-002-crdt-fs-space.md)).

### L0 vs L1-L7 scope

**L0 (active, MVP):**

- 4 MCP-тула: `lofs.create / list / mount / unmount`
- Bucket = OCI repository под `<registry>/lofs/<org>/<bucket>`
- Snapshot = OCI image-manifest (media type `application/vnd.meteora.lofs.snapshot.v1+json`)
- Intent = OCI image-manifest с `subject` на `:latest`, media type `application/vnd.meteora.lofs.intent.v1+json`
- Cooperative coordination через intent manifests + path-scoped writes — полностью в [ADR-002](ADR-002-cooperative-coordination.md)
- Storage: plain tar.zst layer per commit; никакого dedup
- Conflict policy: pull-before-commit + scope-disjoint append + last-writer-wins для пересекающихся файлов (см. ADR-002)

**L1-L7 (future evolution, не в MVP):**

| Layer | Activation trigger | Описание |
|-------|-------------------|----------|
| **L1** | fork+merge workflow в реальном использовании | LLM-driven merge ladder (Identical → Mergiraf → LLM → Human) — см. "Merge system (L1+)" ниже |
| **L2** | storage cost > threshold | Per-file BLAKE3 dedup + reference-counting GC |
| **L3** | dedup ratio < 2× | Content-defined chunking (FastCDC) + pack-files (zstd:chunked) — см. "Chunking + Pack-files (L3+)" ниже |
| **L4** | reads of partial large files | Lazy-mount через SOCI-style zTOC + Range-GET |
| **L5** | долгоживущие архивы | Tiered hot/cold storage (self-hosted hot + S3 Glacier) — см. "Tiered retention (L5+)" ниже |
| **L6** | agent confusion в длинной истории | AI-generated cold-tier summaries + Ed25519 signatures — см. "AI summaries (L6+)" ниже |
| **L7** | первый инцидент от auto-merge | HITL approval policy per sensitive path — см. "HITL hooks (L7+)" ниже |

Все разделы ниже, помеченные `(L1+)` / `(L3+)` / `(L5+)` / `(L6+)` / `(L7+)`, — future work. Их содержание сохранено, чтобы зафиксировать проработанный дизайн, но в MVP не реализуется.

### Объекты

| Объект | Что это | Mutability | L-tier |
|--------|---------|-----------|--------|
| **Bucket** | Named workspace с TTL | mutable head tag | **L0** |
| **Snapshot** | Immutable версия bucket'а (OCI image-manifest) | immutable, content-addressable | **L0** |
| **Intent** | Декларация активной mount-сессии (agent, mode, purpose, scope, heartbeat) как OCI-манифест с `subject → :latest` | lifecycle-managed | **L0** (ADR-002) |
| **Fork** | Новый bucket от чужого snapshot'а | new bucket | **L0** (без merge) |
| **Chunk** | Content-defined chunk (BLAKE3-hashed, FastCDC boundaries) | immutable, globally dedup | L3+ |
| **Pack** | Bundle из N chunks, zstd:chunked compressed, ~16 MiB target | immutable | L3+ |
| **Tree** | Канонически-сериализованная `path → chunk_ref[]` map | immutable | L3+ |
| **MergePlan** | Three-way diff с auto-resolutions + reasoning | transient, reviewable | L1+ |
| **SubMount** | OCI subject-ref от snapshot'а parent bucket'а к snapshot'у другого | часть tree | L1+ |

### Architecture (L0)

```
┌───────────────────────────────────────────────────────────────────────────────┐
│                           Agent runtime                                        │
│  ┌──────────────────────────────────────────────────────────────────────┐    │
│  │  4 MCP tools:  lofs.create  lofs.list  lofs.mount  lofs.unmount       │    │
│  └──────────────────────────────┬───────────────────────────────────────┘    │
└─────────────────────────────────┼────────────────────────────────────────────┘
                                  │ JSON-RPC (stdio / WS)
         ┌────────────────────────▼────────────────────────────────┐
         │                  lofs-daemon (Rust)                      │
         │                                                          │
         │  ┌────────────────┐  ┌──────────────────┐  ┌──────────┐  │
         │  │ Overlay mount  │  │ Coordination     │  │  Registry│  │
         │  │ (fuser +       │  │ (intent          │  │  client  │  │
         │  │  libfuse-fs +  │  │  manifests,      │  │  (oci-   │  │
         │  │  ocirender)    │  │  ADR-002)        │  │  client) │  │
         │  └───────┬────────┘  └─────┬────────────┘  └────┬─────┘  │
         │          │                 │                    │         │
         │          └────────────┬────┴─────────┬──────────┘         │
         │                       │              │                    │
         │             default: OciCoordination │                    │
         │             extension: RedisCoordination / PgCoordination │
         └───────────────────────┼──────────────┼────────────────────┘
                                 │              │
                                 ▼              ▼
                     OCI-compatible registry (Zot / Harbor / GHCR / GitLab)
                     · :latest           → HEAD snapshot-manifest
                     · :intent-<sid>     → active intents (subject → :latest)
                     · :snap-<ts>        → historical snapshot tags
                     · blobs             → tar.zst layers + config JSON
```

**L3+ evolution** дополняет картину CDC-chunking + pack-files + hot/cold tier — см. разделы "Chunking + Pack-files (L3+)" и "Tiered retention (L5+)" ниже. В L0 ни один из этих слоёв не присутствует.

### MCP tools API

**L0 (active MVP) — 4 tools:**

```
lofs.create({ name, ttl_days, size_limit_mb, org? })
    → { bucket_id, name, org, expires_at }

lofs.list({ org?, filter?, include_inactive? })
    → [{ bucket_id, name, org, status, expires_at, size_mb, active_intents[] }]

lofs.mount({ bucket, mode: "ro" | "rw" | "fork",
             purpose, scope?, expected_duration_sec,
             ack_concurrent? })
    → { mount_path, session_id, base_snapshot }
    | MountAdvisory { neighbours[], hints[] }         // see ADR-002

lofs.unmount({ session_id, action: "commit" | "discard",
               message?, conflict_policy? })
    → { new_snapshot_id, parent_snapshot, neighbour_snapshots[] }
    | PushConflict { changed_paths[], hints[] }
```

Argument/return shapes и coordination-семантика — полностью в [ADR-002](ADR-002-cooperative-coordination.md).

**L1-L7 evolution surface (future work, не в MVP):**

```text
# L1 — merge engine (triggered by fork+merge workflow adoption)
merge.propose / merge.auto_resolve / merge.review / merge.suggest /
merge.override / merge.audit_decisions / merge.dry_run / merge.execute /
merge.approve / merge.recover

# L1 — richer reads (triggered by agent demand for history tooling)
ws.read / ws.ls / ws.blame / ws.stat

# L2-L3 — snapshot primitives surfaced (triggered by dedup / CDC rollout)
snap.write / snap.delete / snap.status / snap.commit / snap.list / snap.get
snap.bookmark / snap.unbookmark / snap.list_bookmarks

# L2 — fork surface (triggered by fork+merge workflow adoption)
fork.create / fork.parent / fork.rebase / fork.drift

# L2 — intent lifecycle richer than L0 (triggered by handoff / negotiation demand)
intent.declare / intent.start / intent.heartbeat / intent.update /
intent.complete / intent.abandon / intent.handoff / intent.discover /
intent.who_is_touching / intent.negotiate / intent.escalate_human /
intent.verify_outcome

# L4+ — composition (triggered by monorepo / submodule patterns)
submount.add / submount.update / submount.resolve

# L5 — external sharing (triggered by cross-org artifact flow)
publish.freeze / publish.import / publish.list_shared

# L6 — history / semantic search (triggered by long-history agent confusion)
history.narrative / history.by_agent / history.by_path /
history.semantic_search / history.retrieve_archive
```

Каждая из этих групп — это проработанный дизайн (разделы ниже), активируемый только когда telemetry покажет потребность. MVP targets строго L0.

### Storage layer (L0)

**L0** — **только OCI-реестр**. Никакого S3, MinIO, OpenDAL. Каждый commit = один tar.zst layer, загруженный через `oci-client::push_blob` + новый manifest. Весь dedup в L0 — это OCI-level blob dedup реестра (одинаковые layer-digest'ы не дублируются в storage реестра).

Swap OCI-реестра — через config: `OCI_REGISTRY=http://localhost:5000` для локального Zot, `registry.example/lofs` для remote. [ADR-002](ADR-002-cooperative-coordination.md) фиксирует registry-level coordination.

### Storage layer — hot + cold (L3+ / L5+)

**Hot tier (L3+)** — self-hosted S3-compatible через OpenDAL. Активируется когда dedup ratio < 2× на plain OCI layers и требуется CDC-level dedup. Выбор backend'а plural:

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

### Chunking + Pack-files (L3+)

> ⚠️ **L3 future work.** MVP использует plain tar.zst layer per commit — OCI-level blob dedup достаточен для большинства agent workflows. Секция ниже фиксирует проработанный дизайн CDC + packs для будущей активации, когда telemetry покажет dedup ratio < 2×.

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
- **Pack index** → local SQLite или Postgres (L1+ extension) + `moka` LRU + Bloom filter per-bucket.
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

### Merge system — review-centric (L1+)

> ⚠️ **L1 future work.** MVP разрешает конфликты на commit-time через `conflict_policy` из [ADR-002](ADR-002-cooperative-coordination.md) (reject / scope_merge / fork_on_conflict) — без LLM. Полноценный merge-engine активируется когда fork+merge workflow становится регулярным паттерном.

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

### Intent & claim lifecycle (L1+)

> ⚠️ **L1+ future work.** MVP реализует лёгкий intent-lifecycle через OCI intent-manifests ([ADR-002](ADR-002-cooperative-coordination.md)): mount publishes intent, heartbeat обновляет `heartbeat_at`, unmount удаляет. Полноценный 7-state lifecycle + 10 intent-tools + max-hop enforcement + negotiation rounds — это L1+ evolution, когда MAST-style failure modes начнут реально наблюдаться.

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

### Human-in-the-loop (HITL) hooks (L7+)

> ⚠️ **L7 future work.** MVP не имеет HITL-hook'ов — все actions разрешены агенту. Активация — после первого инцидента от auto-merge или когда команда начнёт работать с sensitive paths (secrets, migrations, CI config).

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

### Tiered retention & AI-generated summaries (L5-L6+)

> ⚠️ **L5-L6 future work.** MVP держит все snapshot'ы в OCI-реестре до TTL expiry (reference-counting GC — L2+). Tiered hot/cold + AI-summaries активируются когда storage cost становится существенным или длинная история начинает путать агентов.

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

### Bookmark mechanism (L5+)

> ⚠️ **L5+ future work.** Bookmarks становятся релевантными вместе с tiered compaction. В L0 все snapshot'ы равнозначны.

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
#### L0 engineering stack (MVP — what ships)

| Компонент | Выбор | Fallback |
|-----------|-------|----------|
| OCI artifact client | **`oci-client`** + **`oci-spec`** | `oras` crate |
| OCI registry primary | **Zot** (local dev + self-hosted, Apache 2.0) | GitLab Container Registry (benchmark target), Harbor |
| Content hashing | **BLAKE3** | SHA-256 |
| Snapshot hashing | **BLAKE3** поверх canonical CBOR | — |
| Canonical encoding | **`ciborium`** (deterministic CBOR) | — |
| Compression (layers) | **zstd-3** | — |
| Tar packing | sync **`tar`** crate (never tokio-tar — CVE-2025-62518) | — |
| Coordination backend (default) | **`OciCoordination`** (intent-manifests via Referrers API) | — |
| FUSE overlay | **`fuser`** + **`libfuse-fs`** (beta, R1) | custom overlay (~1500 LOC fallback) |
| Layer materialization | **`ocirender`** (beta, R2) | plain OCI extract |
| Linux syscalls | **`nix`** + **`caps`** + **`sys-mount`** | — |
| Async runtime | **`tokio`** | — |
| MCP protocol | **`rmcp`** (official Rust SDK) | — |
| CLI framework | **`clap`** v4 derive | — |
| Observability | **`tracing`** + **Prometheus metrics** | — |

**L0 explicitly does NOT ship:** Postgres, Redis, SQLite, OpenDAL, MinIO, S3, FastCDC, zstd:chunked, cosign, Qdrant, DuckDB, Mergiraf. Every single one of those is scoped to a later layer — see table below.

#### L1-L7 extension stack (future)

| Компонент | L-tier | Выбор | Trigger |
|-----------|--------|-------|---------|
| Coordination backend (strong) | L1+ | **Redis 6+** (`SETNX/EXPIRE`, pub/sub) — see ADR-002 | 20+ concurrent rw on same bucket |
| Coordination backend (audit) | L1+ | **PostgreSQL 14+** (`FOR UPDATE SKIP LOCKED`, triggers) — ADR-002 | compliance / audit trail requirement |
| Structured merge | L1 | **Mergiraf** (tree-sitter, 33 langs) via subprocess | fork+merge workflow adoption |
| Op signing | L1+ | **`ed25519-dalek`** | shareable snapshots |
| External artifact signing | L2+ | **`cosign`** (sigstore) | cross-org publish |
| Storage abstraction | L3+ | **OpenDAL** (pin conservative) | CDC/pack layer |
| Chunking | L3 | **`fastcdc` v3.x** (v2020 algorithm) | dedup ratio < 2× |
| Pack format | L3 | **zstd:chunked** (Podman 2026 GA) | reads of partial large files |
| Chunk-index cache | L3+ | **`moka`** (LRU/TinyLFU) | pack hot-path |
| Hot tier | L5 | **MinIO** primary, SeaweedFS, RustFS когда GA | tiered retention |
| Cold storage | L5 | **AWS S3** (IA → Glacier) через OpenDAL | cost > threshold |
| Compression (archive) | L5 | **zstd-19** | cold-tier push |
| Embedding store | L6 | **Qdrant** or **pgvector** | semantic search на summaries |
| Audit query | L6+ | **DuckDB over S3 parquet** | audit log surface |

### Storage layout (L0)

**L0 — один OCI-реестр, один repository на bucket:**

```
<registry>/lofs/<org>/<bucket_name>
├── manifests/
│   ├── :latest                              ← HEAD snapshot-manifest digest
│   ├── :snap-<YYYYMMDDTHHMMSS>              ← historical snapshot tags
│   └── :intent-<session_id>                 ← ephemeral intent-manifests
│                                              (subject → :latest via Referrers API)
│
└── blobs/<sha256>
    ├── <layer tar.zst per commit>
    ├── <config JSON per snapshot>
    └── <intent config JSON>
```

**Snapshot identity** = OCI manifest digest (sha256). Parent link = `subject` field в manifest + `annotations[pro.meteora.lofs.parent_snapshot]` для быстрого walk.

**Intent identity** = OCI manifest digest + `:intent-<session_id>` tag для cleanup. `subject` указывает на `:latest` на момент mount; это автоматически делает intent видимым через Referrers API.

**Никаких локальных файлов состояния, никакой БД.** Весь state in-registry. Daemon-restart / host-crash / disaster recovery тривиальны: поднять daemon, он pull'ит `:latest` + referrers — state восстановлен.

### Storage layout (L3+) — hot + cold tiers

> ⚠️ **L3+ future work.** Секция ниже — эскиз когда потребуется CDC-dedup + tiered retention. В L0 этого нет.

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

В L3+ `snapshot_id` = BLAKE3(canonical_serialize(snapshot)) — Merkle-DAG. Blob-pool **один на org** (не per-bucket), fork от 50 GB bucket'а = килобайты metadata.

## Consequences

### Positive (L0 / L1+)

**L0:**
- ✅ **Zero-infra** — один Zot (или любой OCI-реестр) покрывает весь L0 MVP. Никакого Postgres / Redis на critical path.
- ✅ **Single source of truth** — state в реестре не расходится с БД; disaster-recovery тривиальна.
- ✅ **Offline-capable** — локальный Zot на localhost делает single-host deployment полностью offline.
- ✅ **Mental model git-like** — pull-before-commit, scope-disjoint parallel writes, конфликт = legitimate concern.
- ✅ **OCI-compatible artifacts** — interop с Zot / Harbor / GHCR / GitLab, cosign signing, Referrers API free.
- ✅ **Cooperative multi-writer** — N агентов в дизъюнктных scope'ах работают параллельно без contention (см. ADR-002).
- ✅ **Complexity ядра** ниже чем у CRDT-модели на ~60% (и ниже SQL-first варианта на ~30%).
- ✅ **Нет research-level risk.** Merkle-DAG + CAS + OCI — 20+ лет проверенных паттернов.

**L1+ (при активации):**
- ✅ **LLM как merge-driver** — уникальный differentiator (L1).
- ✅ **Review-centric flow** — agent или human всегда может override, нет "чёрного ящика" (L1).
- ✅ **Binary + large-file native** — CAS не различает, pack-files дёшевы (L3).
- ✅ **Chunk-level dedup** через FastCDC — 2-5× storage savings vs naive per-file blobs (L3).
- ✅ **Hot/cold tiering** — savings 10-25× vs all-S3-Standard на realistic workload (L5).
- ✅ **HITL hooks** — sensitive paths не auto-merge (L7).
- ✅ **Cross-cloud** — OpenDAL ~40 backend'ов бесплатно (L3+).

### Negative

**L0 trade-offs:**
- ❌ **Нет strong pessimistic lock в default-пути** — concurrent rw на одни и те же файлы разрешаются на commit-time. Команды с реальным contention могут нуждаться в `RedisCoordination`/`PostgresCoordination` extensions.
- ❌ **Registry eventual consistency** — редкие corner-case'ы где intent виден не сразу всем. Митигация — overlap detection на commit-time + conflict_policy.
- ❌ **`lofs list` дороже чем SQL** — catalog scan + per-repo manifest pull. Для < 1000 buckets/org приемлемо.
- ❌ **Rate limits** — Docker Hub / GHCR имеют pull-rate-limits. Production нужен self-hosted Zot / Harbor.

**L1+ trade-offs (при активации):**
- ❌ **Нет realtime collaboration** (два агента live в одном файле → fork required) — L0/L1.
- ❌ **Merge conflicts реально случаются** — L1 merge engine.
- ❌ **LLM-merge стохастичен и платный** — tokens cost (L1).
- ❌ **Operational burden от pack-tuning** — experimental iteration (L3).
- ❌ **Compaction теряет intermediate state** — trade-off storage vs full-trace (L5).

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

### Alternative 8: SQL-first coordination (обязательный Postgres в MVP)

Исходная позиция ADR-001 (до v4.1) — Postgres как обязательный backend для metadata и mount-lock'а (шаблон Harbor / Quay / GitLab Container Registry). Rejected в v4.1 для MVP: operational overhead (docker-compose + миграции + connection pool), disaster-recovery усложняется (потеря БД = потеря identity), solo/small-team use-cases реально не требуют strong lock. **Сохранено как `PostgresCoordination` extension-backend** — см. [ADR-002](ADR-002-cooperative-coordination.md). Redis — аналогично, `RedisCoordination` extension, а не обязательная зависимость.

## Implementation

Полный, обновляемый plan с недельной декомпозицией и testing-плана — в [IMPLEMENTATION_PLAN.md](../../IMPLEMENTATION_PLAN.md). Нижеследующий список фиксирует high-level фазы для ADR.

### Phase 0 — Research & design pivot ✅ (2026-04-22)
- [RESEARCH-002](../research/RESEARCH-002-crdt-fs-space.md) CRDT-FS → pivot в fork-merge.
- [RESEARCH-003](../research/RESEARCH-003-layered-bucket-storage.md) CDC + OCI + merge + intent + compaction.
- [RESEARCH-004](../research/RESEARCH-004-rustfs-oci-coordination.md) RustFS+S3 + OCI implementation + MAST coordination.
- [RESEARCH-005](../research/RESEARCH-005-rust-oci-ecosystem.md) Rust OCI ecosystem map.
- [RESEARCH-006](../research/RESEARCH-006-oss-prior-art.md) Competitive landscape.
- [ADR-002](ADR-002-cooperative-coordination.md) — pivot к OCI-only cooperative coordination.

### Phase 1 — L0 MVP (4 MCP tools over OCI, ~6 недель)

Разделено на подфазы в [IMPLEMENTATION_PLAN.md](../../IMPLEMENTATION_PLAN.md):

- **1.1 (week 1-2)** — dev env (docker-compose Zot), CLI skeleton, `lofs.create` + `lofs.list` через registry manifest annotations.
- **1.2 (week 3-4)** — `lofs.mount` rw / ro + `lofs.unmount` commit / discard через intent-manifests + rootless overlay (fuser + libfuse-fs + ocirender).
- **1.3 (week 5)** — path-scoped writes + rich `MountAdvisory` + heartbeat loop + stale-intent GC.
- **1.4 (week 6)** — hardening, observability, benchmark Zot vs GitLab Container Registry, v0.1.0 release.

### Phase 2+ — L1-L7 evolution (data-driven)

Активируется **по метрикам**, а не по календарю. Каждый layer — это feature-flag + extension crate + миграция, не breaking change core'а.

| Layer | Планируемые компоненты | Что триггерит |
|-------|------------------------|---------------|
| **L1** | Merge engine (4-tier ladder, Mergiraf subprocess), fork+merge surface, `RedisCoordination` | fork+merge workflow adoption, 20+ concurrent rw |
| **L2** | Per-file BLAKE3 dedup + reference-counting GC, `PostgresCoordination` + audit triggers | storage cost > threshold, compliance requirement |
| **L3** | FastCDC + pack-files (zstd:chunked) + zTOC lookup, OpenDAL blob backend | dedup ratio < 2× на L2 |
| **L4** | SOCI-style lazy pull + Range-GET для partial reads | reads of partial large files |
| **L5** | Hot/cold tiering (MinIO → S3 IA → Glacier), bookmark mechanism, publish/import cosign | долгоживущие archives |
| **L6** | Cold-tier summarizer (Ed25519-signed summaries, Merkle chain), semantic search | agent confusion в длинной истории |
| **L7** | HITL hooks (`.lofs.toml` approval policy, interrupt на merge/delete/publish, DevBoy UI integration) | первый инцидент от auto-merge |

### Phase 8 — Hardening + observability (2 недели)
- Pack-reorganize (GC + repack).
- Reference-counting GC (weekly sweep).
- OpenTelemetry tracing per-tool.
- Prometheus metrics.
- **MAST-based loadtest** (100 agents × 10 buckets × 14 failure modes).
- Adversarial input fuzzing.
- Spike #5 (hot-tier choice) + Spike #6 (OCI roundtrip) + Spike #7 (MAST injection).

**Total realistic:**
- **L0 MVP (v0.1.0):** ~6 недель.
- **L1-L7 evolution:** 16-19 недель cumulative, **но только по активации**. Многие layer'ы могут никогда не понадобиться.

- **Задачи:** декомпозиция в [IMPLEMENTATION_PLAN.md](../../IMPLEMENTATION_PLAN.md) + GitHub issues per phase.
- **Код:** `meteora-pro/lofs` (OSS Apache 2.0), сабмодуль в DevBoy monorepo.

## References

### Related ADRs
- [ADR-002: Cooperative Coordination Model](ADR-002-cooperative-coordination.md) — OCI-only intent-manifest coordination, extension backends (Redis/Postgres).

### Research documents
- [RESEARCH-002: CRDT-FS space](../research/RESEARCH-002-crdt-fs-space.md) — исходная motivation для pivot.
- [RESEARCH-003: Layered bucket storage](../research/RESEARCH-003-layered-bucket-storage.md) — CDC + OCI + merge + intent + compaction (L3+ reference).
- [RESEARCH-004: RustFS+S3 + OCI + coordination](../research/RESEARCH-004-rustfs-oci-coordination.md) — RustFS maturity + OCI implementation + MAST (historical; coordination section superseded by ADR-002).
- [RESEARCH-005: Rust OCI ecosystem](../research/RESEARCH-005-rust-oci-ecosystem.md) — library selection для L0.
- [RESEARCH-006: OSS prior-art](../research/RESEARCH-006-oss-prior-art.md) — competitive landscape.

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
| 2026-04-22 | Andrey Maznyak | **v4.1 — coordination pivot, L0 cleanup**: OCI-реестр — единственный обязательный backend; SQL/Redis вынесены в опциональные `Coordination` extensions ([ADR-002](ADR-002-cooperative-coordination.md)). Decision section переписан: L0 MVP = 4 MCP tools (`lofs.create/list/mount/unmount`) + intent-manifest cooperative coordination + path-scoped writes. CDC + pack-files, Merge engine, Intent lifecycle (7-state), HITL hooks, Tiered retention, AI-summaries — явно помечены как L1-L7 future evolution, активация по data-driven триггерам. Engineering stack разделён на L0 (OCI + FUSE + tokio) и L1-L7 (opt-in). Storage layout упрощён: один OCI repo per bucket, никаких локальных state-files. Implementation phases сокращены до Phase 1 (L0, ~6 недель) + Phase 2+ (data-driven activation). Offline-capable: локальный Zot делает single-host deployment fully offline. |
