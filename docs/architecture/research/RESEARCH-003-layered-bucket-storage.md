---
id: RESEARCH-003
title: Layered bucket storage — CDC, OCI, merge tooling, intent coordination, tiered compaction
date: 2026-04-22
tags: ["research", "crdt", "cdc", "oci", "merge", "tiered-storage", "agents"]
related_adrs: ["ADR-001"]
---

# RESEARCH-003: Layered bucket storage deep-dive

> Deep-dive для второй итерации [ADR-001](../adr/ADR-001-lofs-crdt.md).
> Focus — 2025-2026 production-grade технологии, явная оценка maturity, actionable deltas для fork-merge модели.

---

## Направление 1 — Content-Defined Chunking (CDC)

### State-of-the-art

- **FastCDC (USENIX ATC'16)** — индустриальный baseline. Gear hash + sub-minimum skipping + normalized distribution. Throughput ~1.95 GB/s single-core (DedupBench benchmarks). FastCDC2020 — stable roll-over, baseline для современных backup-систем.
- **SeqCDC (Middleware'24)** — hashless подход через monotonic sequences. 2300 MB/s boundary judgement + sub-minimum ignore → 1.2-1.35× vs vector-CDC. Maturity: **research → early adoption**.
- **VectorCDC (arXiv 2505.21194, 2025)** — SIMD-accelerated (AVX-512). При chunk-sizes ≥ 16 KB даёт **8.35×–26.2×** throughput vs scalar.
- **Dedup ratio** (DedupBench 2025): FastCDC ≈ Rabin > MaxP > Gear > RAM.
- **Throughput без vector:** FastCDC > Gear > AE-Max > RAM > Rabin.

### Rust crates

- **`fastcdc` v3.1.0** — реализация v2016 + v2020, StreamCDC (Read) + AsyncStreamCDC (tokio/futures features). Production-ready, used in `iroh`, `ouch`. **Выбор №1**.
- SeqCDC — своей crate нет; ~300 LOC портировать вручную если понадобится.

### Pack-file architecture

| Tool | Rolling | Chunk avg | Pack size | Формат |
|------|---------|-----------|-----------|--------|
| **Restic** | Rabin, 64-byte window | 1 MiB (512 KB–8 MB range) | ~4-8 MB | `EncryptedBlob1..N \|\| EncryptedHeader \|\| Len` (header at tail → streaming write) |
| **Borg** | Buzhash | 2 MiB (HASH_MASK_BITS) | ~500 MB segments (append-only) | hashindex cache в C (plain hashtable) |
| **bup** | Rabin-like | 8 KB | packfiles (git-native) | Слишком мелкий target для S3 |
| **casync/desync** | Rabin | 64 KiB | blobs per-chunk (`<hash>.cacnk`) | Хорошо для CAS, плохо для GET-amplification |

### Tuning — content-type-aware профили

| Dataset | Min | Avg | Max | Аргумент |
|---------|-----|-----|-----|----------|
| Код + текст (MD/JSON/YAML) | 2 KB | 8 KB | 32 KB | Много мелких правок, максимальный cross-snapshot reuse |
| Бинарники, дампы, CSV | 64 KB | 1 MiB | 4 MiB | Random-access doesn't dominate, CDC overhead низкий |
| **Mixed (наш default)** | **16 KB** | **256 KB** | **1 MiB** | Компромисс, близко к Restic |

**Heuristic:** read первых 8 KB + MIME-detect → выбрать профиль. Override через `labels` на bucket.

### Read amplification, pack-reorganize

- **Pack-layout:** chunks одного snapshot'а в один pack → один Range-GET.
- **Chunk-index** в Postgres + local LRU (moka) + Bloom filter per-bucket.
- **Range-GET batching** — coalesce adjacent offsets в single HTTP request.
- **Multipart download** — concurrency 8-16 для packs > 100 MB (SOCI pattern).
- **Pack-reorganize** = `git gc`/`repack` для packs. Trigger: pack > 100 MB (split) или pack < 4 MB старше 7 дней (merge).

### Antipatterns (когда CDC вреден)

1. Очень мелкие файлы (< 4 KB) — inline в tree.
2. Encrypted/compressed content — chunk'ать **до** шифрования.
3. Media с re-encoded frames — CDC не поможет, нужен semantic dedup.
4. Append-only logs — лучше fixed-offset + tail append.

### Actionable для lofs

1. **`fastcdc` v2020** default с content-type profiles.
2. **Pack-file format:** Borg-style segments ~16 MB, header at tail, zstd inside.
3. **Chunk index:** Postgres + moka LRU + Bloom filter.
4. **Range-GET batching** + multipart для больших файлов.
5. **Отложить SeqCDC/VectorCDC** до sustained > 1 GB/s bottleneck.

**Ссылки:**
- [FastCDC USENIX ATC'16](https://www.usenix.org/system/files/conference/atc16/atc16-paper-xia.pdf)
- [SeqCDC Middleware'24](https://cs.uwaterloo.ca/~alkiswan/papers/SeqCDC_Middleware24.pdf)
- [VectorCDC arXiv 2505.21194](https://arxiv.org/html/2505.21194v1)
- [`fastcdc` Rust crate](https://crates.io/crates/fastcdc)
- [Restic design](https://restic.readthedocs.io/en/latest/design.html)
- [Borg internals](https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html)
- [casync](https://github.com/systemd/casync) · [desync](https://github.com/folbricht/desync)
- [DedupBench](https://github.com/UWASL/dedup-bench)

---

## Направление 2 — OCI layer model, применимость к fork/merge-DAG

### State

**OCI Image Spec v1.1 (GA март 2024)** — зрелый production standard.

### Что берём из OCI

1. **Descriptor + digest** — content-addressable, прямой аналог BLAKE3 CAS.
2. **Manifest = root объект** (config + layer descriptors) — аналог Snapshot.
3. **Subject-reference (v1.1)** — `subject: { digest }` для связки artifacts → прямая модель SubMount.
4. **Range-GET + chunk index** (SOCI-style) — random access к pack'ам.
5. **Parallel pull** (SOCI 2025) — concurrent fetch + unpack.
6. **Stargz TOC-inside-pack** — random access без external metadata.
7. **BuildKit LLB** — content-addressable DAG operations, пример для merge-cache.

### Lazy pulling

- Harter et al. (FAST'16): **76% image payload pulls, только 6.4% used**.
- **stargz-snapshotter / eStargz** (NTT) — HTTP Range из tar.gz.
- **zstd:chunked** (Podman) — zstd вместо gzip, GA январь 2026.
- **SOCI (AWS)** — external index, parallel pull mode 2025.
- **Nydus/RAFS/EROFS** (Alibaba) — chunk-level dedup, CNCF Sandbox.

### Layer dedup в registries

- Harbor — storage-level через hard-links (local FS only).
- Zot — startup-enforced dedupe.
- Distribution — CAS по SHA-256 idempotent PUT.

### CRIU для agent state

- CRIUgpu (2025) — NVIDIA cuda-checkpoint integration.
- Kubelet checkpoint API default в k8s 1.30.
- **Применимость:** checkpoint долгой agent task как lofs-blob. Orthogonal to snapshot-tree, **watch-list**, не в MVP.

### Что НЕ брать

1. ❌ Union-FS semantics (copy-up).
2. ❌ Linear parent-chain — нам нужен DAG.
3. ❌ Whiteout file convention — tombstones в tree proper.
4. ❌ Distributable vs non-distributable layers (deprecated).

### Actionable

1. **Snapshot → OCI-like manifest** (`application/vnd.meteora.lofs.snapshot.v1+json`). Interop bonus: push в Zot/Harbor.
2. **Subject-references** для SubMount — стандартизируем.
3. **SOCI parallel range-GET** для `ws.read` больших файлов.
4. **Spike #2** предложен: export snapshot в OCI artifact, push/pull roundtrip.

**Ссылки:**
- [OCI Image Spec v1.1](https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md)
- [OCI 1.1 announcement](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/)
- [stargz-snapshotter](https://github.com/containerd/stargz-snapshotter)
- [SOCI Parallel Pull 2025](https://aws.amazon.com/blogs/containers/introducing-seekable-oci-parallel-pull-mode-for-amazon-eks/)
- [Nydus/RAFS](https://nydus.dev/)
- [BuildKit 2025](https://github.com/moby/buildkit)
- [CRIU + Kubernetes 2026](https://oneuptime.com/blog/post/2026-02-09-container-checkpoint-restore-criu/view)
- [CRIUgpu 2025](https://www.devzero.io/blog/gpu-container-checkpoint-restore)

---

## Направление 3 — Advanced merge + review tooling

### Инструменты

- **Mergiraf** (LWN 2025: «становится стандартом») — tree-sitter-based merge driver. **33 языка**. GPL-3.0. Rust. Production-ready. Fallback на AST parse только для конфликтов → быстро.
- **Weave** (Ataraxy-Labs, 2026) — entity-level merge. Benchmark: **31/31 clean vs git's 15/31**, **500 merges git/git repo — 0 regressions**. Production-intended, early-adopter.
- **LastMerge** (arXiv 2507.19687, 2025) — language-agnostic через tree-sitter (350+ языков), 15% fewer false positives vs jDime.
- **GumTree + refactoring-aware AST diff** (TOSEM 2024) — baseline для большинства tools.
- **Git merge-ort** (default 2.33+) — 500× speedup, rename-caching.

### LLM-driven merge в production

- **GitHub Copilot (GA апрель 2026)** — "Fix merge conflicts in three clicks", agentic flow в cloud env (edit + tests + push).
- **Cursor** — `Resolve in Chat`, Cursor Agent с understand-both-sides.
- **Aider** — BYOM open-source.
- **LLMinus (Linux kernel, 2024)** — **RAG-over-history**: semantic embeddings find similar past conflicts → retrieve resolution patterns. Очень интересный pattern для нас.

### Structured merge (JSON/YAML/TOML)

- **yq** — `*` operator для deep merge (но shallow для arrays, и НЕ three-way).
- **jq** — аналогично.
- **Mergiraf --language=json|yaml|toml** — three-way structured. Наш выбор.

### AI code review — prior art

| Tool | Подход | Benchmark 2025 |
|------|--------|---------------|
| CodeRabbit | LLM + AST context | 44% catch-rate |
| Greptile | Full-repo vector embeddings | **82% catch-rate** |
| Bugbot (Cursor) | Project context | 58% |
| GitHub Copilot CCR | LLM + tool-calling | (не bench'ено) |
| Bito | Claude + knowledge graph multi-repo | (не bench'ено) |
| Codeball | ML на historical PR data | — |

Trade-off: Greptile топ по catch-rate, но больше false-positives.

### Merge conflict taxonomies

1. **Trivial** — identical / whitespace → auto-resolve.
2. **Structural** — same entity edited differently → Mergiraf/Weave.
3. **Semantic** — renames/refactors/type changes → tests-in-loop or LLM-with-build-feedback.
4. **Functional** — behavior changes contradict → quarantine + flag.
5. **Interaction** — unrelated changes together break → integration testing.

### Actionable — 4-tier MergeStrategy

```rust
enum MergeStrategy {
    // Tier 1 — cheap, exact
    Identical,
    SideOnly,
    LwwByTimestamp,

    // Tier 2 — syntax-aware (cheap, deterministic)
    TreeSitterMergiraf,
    StructuredYq,
    TreeSitterWeave,

    // Tier 3 — semantic (expensive, LLM-backed)
    AgentDriven {
        model: String,
        dual_verify: bool,  // second LLM cross-check
        tools: Vec<Tool>,   // read_file, run_test, show_blame, diff
    },

    // Tier 4 — escalate
    HumanReview,
}
```

### Merge-agent toolset (8 tools)

1. `ws.read(path, snapshot_id)` → bytes
2. `ws.blame(path, snapshot_range)` → history
3. `merge.threeway_diff(path)` → structured diff
4. `merge.related_changes(entity_name)` → related files via AST-index
5. `merge.run_check(path)` → `{syntax_ok, type_ok, test_result}`
6. `merge.ask_peer_agent(source_snapshot, question)` → text
7. `merge.quarantine(content)` → blob_ref
8. `merge.dry_run(proposed_merge)` → `{conflicts_count, warnings}`

### Risks

- **LLM-hallucinated merge** — dual-model verify + run_check + quarantine.
- **Mergiraf GPL-3.0** — spawn subprocess, не static-linking. Apache 2.0 compatible via exec-boundary.
- **Tree-sitter grammars versioning** — pin в Cargo.lock.

**Ссылки:**
- [Mergiraf](https://mergiraf.org/architecture.html) · [source on Codeberg](https://codeberg.org/mergiraf/mergiraf) · [LWN 2025](https://lwn.net/Articles/1042355/)
- [Weave](https://github.com/ataraxy-labs/weave)
- [LastMerge arXiv 2507.19687](https://arxiv.org/abs/2507.19687)
- [GumTree](https://github.com/GumTreeDiff/gumtree) · [TOSEM 2024](https://dl.acm.org/doi/10.1145/3696002)
- [LLMinus](https://lwn.net/Articles/1053714/)
- [GitHub Copilot merge conflicts 2026](https://github.blog/changelog/2026-04-13-fix-merge-conflicts-in-three-clicks-with-copilot-cloud-agent/)
- [Cursor AI merge conflicts](https://docs.cursor.com/more/ai-merge-conflicts)
- [Greptile benchmarks 2025](https://www.greptile.com/benchmarks)
- [State of AI Code Review 2025](https://www.devtoolsacademy.com/blog/state-of-ai-code-review-tools-2025/)

---

## Направление 4 — Intent metadata + multi-agent coordination

### State

- **A2A Protocol v0.3 (Google, July 2025, 150+ orgs)** — Task + Artifact + Agent Card. v0.3 добавил gRPC, signed security cards, Python SDK. **Directly applicable schema для Intent.**
- **MetaGPT** — role-based pipeline (PM → Architect → Engineer → QA → Reviewer).
- **AutoGen (Microsoft)** — sequential/concurrent patterns, coordinator-driven handoffs.
- **CrewAI** — role-driven crews, production-focused DX.
- **OpenAI Agents SDK (2025)** — stateless handoffs с full context. No hidden variables. LOFS закрывает persistent-state pain.
- **LangGraph Supervisor** — hierarchical routing, reassignment-on-validation.

### Soft advisory locks

- **Postgres advisory locks** — `pg_try_advisory_lock`, session-scoped, cheap. Production-standard.
- **fcntl / POSIX** — local-only.
- **Gossip** (arXiv 2508.01531, 2025) — academic для agent emergent coordination, не для critical sync.

### Claim-based в production 2026

- Mike Mason (Jan 2026): **57% компаний run AI agents в prod**.
- Anthropic (2025-2026) — 5 patterns: orchestrator-subagent, shared observable state, message bus, agent-team, claim-based queue.
- **CodeCRDT (arXiv 2510.18893, Oct 2025)** — observation-driven coordination через shared CRDT state. **+15-30% task completion vs message-passing**, но complexity выше. Watch как потенциальный competitor.

### Long-running task

- **Temporal.io** — durable workflows, exactly-once, fault-tolerant actors. Production-standard для agent orchestration 2025.
- **Airflow** — batch/scheduled, не для continuous.
- **Cadence** — Uber's predecessor, OSS.

### Actionable — Intent + coordination API

```
# Decl на fork'е
fork.create(
  source_snapshot,
  intent: IntentSpec {
    goal: String,                       // "implement rate limiting in API gateway"
    scope: Vec<PathGlob>,               // ["apps/api/src/middleware/**"]
    estimated_duration: Duration,
    declared_by: AgentId,
    blocks_on: Option<Vec<TaskRef>>,    // зависимости
    labels: BTreeMap<String, String>,   // {task_id: "DEV-666"}
  },
)

# Soft advisory
intent.who_is_touching(bucket, path) → [AgentId + declared_intent]
intent.start(bucket)
intent.heartbeat(bucket)  // каждые 60с, TTL 5мин
intent.complete(bucket)
intent.abandon(bucket)
intent.expire(bucket)     // auto-release без heartbeat
intent.discover(org, filter) → [active_intents]
```

**Soft — не hard.** Overlapping intents allowed, но warning. Агент сам решает.

### Риски

- **Soft advisory игнорируется** — митигация: `bucket.stat` shows warnings в MCP tool description (LLM читает).
- **Intent drift** — declared "A", done "B". Митигация: LLM comparing intent.goal vs commits.messages, flag mismatch.
- **Deadlock на cross-bucket `blocks_on`** — DAG-валидация (как SubMount cycles).

**Ссылки:**
- [A2A Protocol Specification](https://a2a-protocol.org/latest/specification/) · [GitHub](https://github.com/a2aproject/A2A)
- [Anthropic Multi-Agent Coordination Patterns](https://claude.com/blog/multi-agent-coordination-patterns)
- [LangGraph Supervisor](https://github.com/langchain-ai/langgraph-supervisor-py)
- [OpenAI Agents SDK cookbook](https://cookbook.openai.com/examples/orchestrating_agents)
- [CrewAI vs LangGraph vs AutoGen](https://www.datacamp.com/tutorial/crewai-vs-langgraph-vs-autogen)
- [Postgres advisory locks](https://rclayton.silvrback.com/distributed-locking-with-postgres-advisory-locks)
- [Temporal vs Airflow 2025](https://sparkco.ai/blog/temporal-vs-airflow-agent-orchestration-showdown-2025)
- [CodeCRDT arXiv 2510.18893](https://arxiv.org/pdf/2510.18893)
- [AI Coding Agents in 2026 — Mike Mason](https://mikemason.ca/writing/ai-coding-agents-jan-2026/)
- [Keyhive — Ink & Switch](https://www.inkandswitch.com/keyhive/notebook/)

---

## Направление 5 — Tiered compaction + AI-generated summaries

### Prior art

- **Kafka log compaction** — tombstone-based, key-based retention. Kafka 4.x (2025) smarter compaction.
- **Pulsar/Redpanda** — tiered S3 storage, compaction через streaming.
- **Git GC + reflog** — cruft packs (2.37+), unreachable в отдельный pack. `gc.reflogExpire` = 90d reachable, 30d unreachable.
- **Restic retention** — `keep-daily/weekly/monthly/yearly` + `forget` + `prune`.
- **Borg prune** — soft-delete → `borg compact` физически освобождает. **`borg undelete`** до compact. Более user-friendly.
- **ZFS holds + auto-snapshot** — `zfs hold <tag>` prevent destroy, cron-rotation labels.
- **RocksDB LSM compaction** — leveled (low space amp, high write amp) vs universal (burstier, lower write amp).

### S3 tiered pricing

- **S3 Intelligent-Tiering:** auto-move 30d → IA (-40%), 90d → Archive Instant (-68%), 180d → Deep Archive (-95%). Monitoring $0.0025/1000 objects/month. No retrieval charges.
- **Glacier:** Instant $0.004/GB, Flexible $0.01-0.03/GB + hours retrieval, Deep Archive $0.00099/GB + 12h.
- **R2** — flat, no IA tier.
- **GCS** — Nearline/Coldline/Archive аналог S3.

### Semantic summarization

- **GitHub Copilot PR summary** — overview + bulleted changes.
- **Release Please** — deterministic, rule-based (Conventional Commits).
- **Changesets** — monorepo explicit changesets в CHANGELOG.
- **LLM Release Action** — semantic version bump + multi-audience changelogs.
- **arXiv 2603.14619 (LLM-Augmented Release Intelligence)** — pipeline filters routine commits before LLM.
- **MemGPT / Letta** — recursive summary memory block, multi-tier (core/conversational/archival/recall). **Reflection subagents** на compact. **Прямо применимо к cold-tier summary**.
- **MemPalace (2026)** — 5-layer memory (wings/halls/rooms/closets/tunnels). 170 tokens L0+L1. **60.9% → 94.8% retrieval accuracy через scope-narrowing**. Hierarchical summaries → dramatically better retrieval.

### Retrieval-over-summary

- **Vector embeddings** → Qdrant (filter + similarity) или pgvector.
- **Full-text fallback** — Postgres tsvector.
- **FAISS** — только vector, без filter.

### Cost analysis (1 TB / 1 год / 100 snapshots/day)

Допущения: 36500 snapshots/year, avg delta 100 MB → 10 TB с CDC dedup.

| Scenario | Cost/год | Savings |
|----------|----------|---------|
| A: Всё в S3 Standard | $2760 | baseline |
| B: Tiered (hot+warm+cold summary) | **$380** | **7× дешевле** |

B breakdown:
- Hot (7d, ~200 GB) S3 Standard: $55
- Warm (30d, ~2 TB) S3 Standard-IA: $300
- Cold summary (34 GB) Glacier Instant: $1.6
- Archive escape (2 TB) Deep Archive: $24

### AI-summary schema (v1)

```json
{
  "schema_version": "v1",
  "period": { "start_snapshot", "end_snapshot", "start_ts", "end_ts", "snapshots_consolidated" },
  "agents": [{ "agent_id", "snapshot_count" }],
  "changed_paths": [{ "glob", "change_kind", "lines_delta" }],
  "narrative": {
    "summary_short": "≤ 200 chars",
    "summary_full": "10-30 sentences, structured",
    "key_decisions": ["..."]
  },
  "bookmarked_snapshots": [{ "snapshot_id", "reason", "held_by" }],
  "omitted_blobs": {
    "count", "total_size_bytes",
    "archive_pointer": "s3://.../period.tar.zst",
    "archive_manifest": "blob_hash_index.json"
  },
  "signatures": {
    "summary_hash_blake3", "prev_summary_hash_blake3",
    "signed_by", "signature"
  },
  "embedding": { "model", "vector_ref" }
}
```

### Compaction triggers

| Trigger | Default | Action |
|---------|---------|--------|
| Time-based | Snapshots > 30 дней | warm consolidation |
| Count-based | > 200 snapshots в bucket | week-packs |
| Size-based | Bucket > 10 GB | force tier-check |
| Explicit | `bucket.compact(until)` | manual |
| Bookmark protection | `held_by != null` | skip from consolidation |

### Default retention

```toml
[retention]
hot_snapshots_count = 20
hot_max_age_days = 7
warm_max_age_days = 30
warm_consolidation_step = "weekly"
warm_storage_class = "S3-Standard-IA"
cold_start_age_days = 60
cold_mode = "summary_only"
cold_storage_class = "Glacier-Instant"
archive_storage_class = "Glacier-Deep-Archive"
archive_retrieval_sla_hours = 12
bookmark_ttl = "never"
```

### Bookmark mechanism

```
snap.bookmark(snapshot_id, label?, ttl?)
snap.unbookmark(snapshot_id)
snap.list_bookmarks(bucket)
```

ZFS-hold pattern: `bookmark: {label, held_by, ttl}` в snapshot metadata. Excluded from compaction.

### Risks / gotchas

- **Cold summary теряет context** — dual-model verify + bookmark + archive-pointer + cryptographic signing.
- **Recursive-summarization drift** — anchor на hot snapshots если живы.
- **Storage class transitions** — monitoring fee $0.0025/1000 objects, transition fee $0.01-0.03/GB. Минимизировать retrievals через embeddings-over-summary.
- **Embedding model drift** — version pinning, реиндексация при major upgrade.
- **Summary hallucination** — validation через diff-стат + keyword-check.

**Ссылки:**
- [Kafka Log Compaction](https://docs.confluent.io/kafka/design/log_compaction.html) · [2025 Edition](https://cloudurable.com/blog/kafka-architecture-log-compaction-2025/)
- [Pulsar tiered](https://pulsar.apache.org/docs/2.4.2/concepts-tiered-storage/) · [Redpanda tiered](https://docs.redpanda.com/current/manage/tiered-storage/)
- [Git GC](https://git-scm.com/docs/git-gc) · [Git Repack](https://git-scm.com/docs/git-repack)
- [Restic forget+prune](https://restic.readthedocs.io/en/stable/060_forget.html) · [Borg prune](https://borgbackup.readthedocs.io/en/latest/usage/prune.html)
- [Vadim Panin's restic Glacier Deep Archive](https://vadim.ai/blog/2025-12-29-restic-s3-deep-archive/)
- [ZFS Snapshot Holds](https://docs.oracle.com/cd/E19253-01/819-5461/gjdfk/index.html) · [zfs-auto-snapshot](https://github.com/zfsonlinux/zfs-auto-snapshot)
- [RocksDB Leveled](https://github.com/facebook/rocksdb/wiki/Leveled-Compaction) · [Universal](https://github.com/facebook/rocksdb/wiki/Universal-Compaction)
- [S3 Intelligent-Tiering](https://docs.aws.amazon.com/AmazonS3/latest/userguide/intelligent-tiering-overview.html) · [2026 S3 Pricing Guide](https://www.cloudzero.com/blog/s3-pricing/)
- [MemGPT/Letta memory](https://docs.letta.com/advanced/memory-management/) · [MemGPT paper](https://arxiv.org/abs/2310.08560)
- [MemPalace](https://github.com/MemPalace/mempalace)
- [Release Please](https://github.com/googleapis/release-please) · [LLM Release Intelligence arXiv 2603.14619](https://arxiv.org/html/2603.14619)
- [APCE — AI-Powered Commit Explorer arXiv 2507.16063](https://arxiv.org/html/2507.16063v1)
- [Qdrant vs FAISS](https://zilliz.com/comparison/qdrant-vs-faiss)

---

## Финальные рекомендации

### Top-10 must-read sources

**P1 — до Phase 1:**
1. [FastCDC USENIX ATC'16](https://www.usenix.org/system/files/conference/atc16/atc16-paper-xia.pdf)
2. [Restic design docs](https://restic.readthedocs.io/en/latest/design.html)
3. [Mergiraf architecture](https://mergiraf.org/architecture.html)
4. [OCI Image Spec v1.1](https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md)

**P2 — в Phase 2-3:**
5. [Weave (Ataraxy-Labs)](https://github.com/ataraxy-labs/weave)
6. [A2A Protocol v0.3](https://a2a-protocol.org/latest/specification/)
7. [MemGPT paper](https://arxiv.org/abs/2310.08560)
8. [Anthropic Multi-Agent Coordination Patterns](https://claude.com/blog/multi-agent-coordination-patterns)

**P3 — watch-list:**
9. [SeqCDC Middleware'24](https://cs.uwaterloo.ca/~alkiswan/papers/SeqCDC_Middleware24.pdf) — если CDC станет bottleneck
10. [CRIUgpu 2025](https://www.devzero.io/blog/gpu-container-checkpoint-restore) — долгосрочная perspective

### Топ-5 рисков дизайна

1. **LLM-summary hallucination** в cold tier → тихая потеря context'а. *Митигация:* dual-model verify + bookmark + archive-pointer recovery + Ed25519-signed summaries.
2. **Mergiraf GPL-3.0** vs наш Apache 2.0 → static linking невозможен. *Митигация:* subprocess + stdin/stdout interface (exec-boundary).
3. **Soft advisory игнорируется** агентами → no heartbeat. *Митигация:* bucket.stat показывает warnings в MCP tool descriptions (LLM reads).
4. **Pack-file tuning — operational expertise** → нет universal answer. *Митигация:* Phase 7 loadtest + tuning, metrics с day 0.
5. **Snapshot-DAG drift на длительных forks** → merge-plan overflow LLM context. *Митигация:* hard-stop > 200, incremental rebase, split MergePlan per-subtree, auto-suggest rebase при > 50.

### Deltas vs ADR-001 (обновлённый стек)

**Добавить:**

1. `fastcdc` crate (v2020) с content-type profiles, pack-size 16 MiB.
2. Pack-file format (Borg-style segments, header tail, zstd inside).
3. `moka` crate — LRU/TinyLFU cache.
4. `qdrant-client` или pgvector для embedding-over-summary.
5. `ed25519-dalek` расширить scope на cold-tier summaries.
6. Intent metadata API (fork.create + intent.* MCP tools), Postgres table + Redis active state.
7. Bookmark mechanism (`snap.bookmark/unbookmark/list_bookmarks`).
8. Cold-tier summary generator (новый crate `lofs-summarizer`).

**Обновить:**

9. MergeStrategy — 4-tier ladder (Tier 1 cheap → Tier 4 escalate).
10. Merge-agent toolset — 8 formal tools (read, blame, threeway_diff, related_changes, run_check, ask_peer_agent, quarantine, dry_run).

**Заменить/углубить:**

11. OpenDAL с pack-файлами, не blob-per-object. Снижает S3 API cost 10-100×.
12. Snapshot serialization — OCI-manifest-compatible JSON. Interop с Zot/Harbor.

**Phase-plan расширяется** с 9-12 до **~14-16 недель**:
- Phase 1.5 (новая): pack-file + CDC tuning (1-2 недели).
- Phase 3.5 (новая): merge-agent toolset formal interface (1 неделя).
- Phase 7.5 (новая): cold-tier summarizer + bookmarks (2 недели).

### Watch-list

- SeqCDC / VectorCDC — при sustained > 1 GB/s bottleneck.
- CRIU integration — для "pause agent" feature.
- Property-preserving merge (tests-in-loop) — research, возврат 6-12 мес.
- Keyhive/Beelay E2EE — при SaaS multi-tenant.
- A2A full interop — Phase 5+3 мес после API stabilized.
- OCI artifact push/pull — Phase 6 spike.
- CodeCRDT observation-driven — мониторим competitor.

### Дополнительные spikes

1. **Spike #2** — Pack-file benchmark на 1 TB mixed-workload, pack-size tuning (1-2 недели).
2. **Spike #3** — Merge-agent toolset validation на 100 synthetic conflicts (2 недели).
3. **Spike #4** — Summary generation на реальном bucket (500 snapshots), retrieval validation (1 неделя).

### Резюме

ADR-001 fork-merge модель — концептуально правильная. Research не требует pivot'а, но указывает на **5 deltas в стеке** и **3 новые подсистемы** (intent, bookmarks, cold-tier summarizer). Самая новая и уязвимая идея — cold-tier AI summaries без blob'ов — требует спайка на реальном workload перед фиксацией. Остальное — инженерная реализация зрелых паттернов.
