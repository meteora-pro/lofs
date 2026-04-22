---
id: RESEARCH-004
title: Deep-dive round 2 — RustFS+S3, OCI artifacts, multi-agent coordination
date: 2026-04-22
tags: ["research", "storage", "rustfs", "oci", "multi-agent", "coordination"]
related_adrs: ["ADR-001"]
---

# RESEARCH-004: RustFS+S3, OCI artifacts, multi-agent coordination

> Углубление [RESEARCH-003](RESEARCH-003-layered-bucket-storage.md) по трём направлениям, где первая итерация дала overview, а не implementation-level детали. Production-focus 2025–2026.

---

## Направление 1 — RustFS hot + S3 mirror cold

### 1.1 Maturity RustFS (критично!)

- **RustFS — alpha (v1.0.0-alpha, начало 2026), distributed mode официально НЕ GA.** Документация явно предупреждает *"do NOT use in production environments"*.
- 104 контрибьютора, активное развитие, но не battle-tested at scale. Maturity: **early-adopter / risky-for-prod**.
- **Performance asymmetrия:** на 4-node кластере с 20 MiB объектами MinIO даёт ~53 Gbps при TTFB 24ms, RustFS ~23 Gbps при TTFB 260ms. На 4 KB payload'ах RustFS 2.3× быстрее MinIO (mixed read/write/delete). На pure GET MinIO значительно выигрывает.

**Recommendation:** hot-tier = **plural через OpenDAL** (MinIO primary → SeaweedFS alternative → RustFS когда distributed GA, ожидается 2026 H2). OpenDAL делает swap прозрачным.

**Сравнение self-hosted S3 2026:**

| Tool | Maturity | Лицензия | Фокус |
|------|----------|----------|-------|
| **MinIO** | ✅ battle-tested | AGPL-3.0 (+ commercial) | General-purpose, production default |
| **SeaweedFS** | ✅ production | Apache 2.0 | Volume-packed small files, O(1) reads — хорош для pack-файлов |
| **Garage** | ✅ production | AGPL-3.0 | Geo-distributed small clusters |
| **Ceph** | ✅ gold standard | LGPL | Petabytes, high operational burden |
| **RustFS** | ⚠️ alpha | Apache 2.0 | Targeting MinIO replacement |

### 1.2 Chunk и pack tuning

- RustFS использует **фиксированный 1 MiB block size для Reed-Solomon EC** (Vandermonde variant). Pack < 1 MiB даст write-amplification. 
- **Pack-file 16 MiB = 16 RS-блоков** — идеально для обоих: RustFS (aligned) + S3 (multipart min 5 MiB).
- Erasure coding default **4+2** (33% overhead) — достаточно, т.к. cold-mirror даёт дополнительную durability. Не тянуть 8+4 (50% overhead).
- **Compression:** **zstd-3 на hot** (2.5× compress, быстрый decompress = cheap reads), **zstd-19 на archive** (4× compress, 6-10× slower compress, decompress одинаков).

### 1.3 Chunk profiles (по типу)

| Profile | Min | Avg | Max | Use |
|---------|-----|-----|-----|-----|
| **code** | 2 KB | 8 KB | 32 KB | text/config/MD/JSON/YAML — aggressive dedup |
| **mixed (default)** | 16 KB | 256 KB | 1 MiB | unknown content type |
| **binary** | 64 KB | 1 MiB | 4 MiB | dumps, parquet, zip — random access dominant |

Heuristic: MIME-detect по first 8 KB + override через `labels` на bucket.

### 1.4 RustFS → S3 mirror replication

- **RustFS bucket replication** (active-active, sync/near-sync). Между двумя DC, не multi-site. RTT < 20 ms, loss < 0.01%.
- **Lifecycle policies** на RustFS с `transition after N days` → hot_tier auto-transitions в S3 cold.
- Альтернатива: **MinIO bucket replication** (`mc replicate add`) — production-proven.

### 1.5 Read-through fallback

RustFS lifecycle expires в hot, но **не делает transparent read-through к S3**. Вариант:
- **Application-level fallback** (naш lofs-daemon): hot 404 → S3 fetch → repopulate hot (write-back cache). Clean, контроль у нас.
- Два OpenDAL backend'а: `hot_op` + `cold_op`, cascade в BlobStore.

### 1.6 Failure modes

- RustFS 4+2 EC: survives 2 node failures, auto-heal on return.
- Total cluster down: S3 mirror read-only fallback (чтение). Write блокируется до восстановления.
- S3 mirror lag (async): window seconds. Митигация — **дублированная запись** для критичных ops (commit, merge), acceptable lag для обычных pack'ов.

### 1.7 Cost model (10 TB hot + 10 TB mirror)

- **RustFS/MinIO hot on-prem:** $50-150/мес (Hetzner dedicated, k8s volumes).
- **S3 cold 10 TB:**
  - Standard-IA: ~$125/мес
  - Intelligent-Tiering после 90d: ~$60-100/мес
  - Glacier Instant: ~$40/мес
- **Egress** (once для replication, потом deltas).
- **Total: $100-250/мес** (vs $2760 all-S3-Standard из RESEARCH-003). **Savings 10-25×.**

### 1.8 Observability

RustFS (и MinIO): Prometheus metrics out-of-box, Grafana dashboards ship, OpenTelemetry traces. Application-level CAS-hit-ratio / pack-fill / merge-conflict-rate — на нашей стороне.

### 1.9 Actionable TOML defaults

```toml
[storage]
hot_backend = "s3_compat"            # minio|rustfs|seaweedfs via OpenDAL
hot_endpoint = "${HOT_S3_ENDPOINT}"
hot_bucket = "lofs-hot"

cold_backend = "s3"
cold_endpoint = "s3.amazonaws.com"
cold_bucket = "lofs-cold"
cold_storage_class = "INTELLIGENT_TIERING"

replication_mode = "async"
replication_lag_alert_sec = 120

[pack]
size_target = "16 MiB"               # align 16×RS-blocks
size_min = "4 MiB"
size_max = "64 MiB"
compression_hot = "zstd-3"
compression_archive = "zstd-19"

[chunking.mixed]                     # default
min = "16 KiB"
avg = "256 KiB"
max = "1 MiB"

[chunking.code]
min = "2 KiB"
avg = "8 KiB"
max = "32 KiB"

[chunking.binary]
min = "64 KiB"
avg = "1 MiB"
max = "4 MiB"
```

### 1.10 Risks

- ⚠️ **RustFS alpha** — используем MinIO primary до RustFS GA. OpenDAL swap прозрачен.
- ⚠️ **1 MiB RS block** — pack < 4 MiB дадут write amplification.
- ⚠️ **Cross-cloud egress** — hot и cold в одном AZ/region.
- ⚠️ **RTT < 20 ms** для active-active replication — топология ограничена.

### Ссылки Направление 1
- [RustFS GitHub](https://github.com/rustfs/rustfs) · [rustfs.com](https://rustfs.com/) · [Docs](https://docs.rustfs.com/) · [Lifecycle](https://docs.rustfs.com/features/lifecycle/) · [Replication](https://docs.rustfs.com/features/replication/) · [Erasure Coding](https://deepwiki.com/rustfs/rustfs/5.2-erasure-coding-and-data-protection)
- [RustFS Benchmark discussion](https://github.com/rustfs/rustfs/issues/2154)
- [Self-hosted S3 2026 (Rilavek)](https://rilavek.com/resources/self-hosted-s3-compatible-object-storage-2026) · [MinIO Alternatives 2026 (Akmatori)](https://akmatori.com/blog/minio-alternatives-2026-comparison)
- [MinIO bucket replication](https://min.io/docs/minio/linux/administration/bucket-replication.html) · [S3 Cross-Region Replication](https://docs.aws.amazon.com/AmazonS3/latest/userguide/replication.html)

---

## Направление 2 — OCI artifact push/pull

### 2.1 Media types (vendor-prefix pattern)

IANA registration — **не требуется для MVP**. Helm ждал 4 года до IANA (2018→2022); WASM OCI не регистрировали вообще. Vendor-prefix `vnd.devboy.*` достаточен.

**Предлагаем для lofs:**

```
application/vnd.meteora.lofs.snapshot.v1+json     # root manifest
application/vnd.meteora.lofs.tree.v1+cbor         # path → blob_hash map
application/vnd.meteora.lofs.pack.v1.zst          # pack-layer (zstd bundle)
application/vnd.meteora.lofs.mergeplan.v1+json    # merge plan (attached via subject)
application/vnd.meteora.lofs.auditlog.v1+parquet  # audit segment
application/vnd.meteora.lofs.summary.v1+json      # cold-tier AI summary
```

### 2.2 Rust crates

| Crate | Роль | Maturity |
|-------|------|----------|
| **`oci-client`** (бывш. `oci-distribution`) | HTTP push/pull, auth | Production, active (wasmtime, krustlet) |
| **`oci-spec`** | Spec types (Runtime/Image/Distribution) | Production |
| **`rust-oci-client`** (oras-project) | Higher-level wrapper | Active |
| **`oras`** | ORAS ops (Range GET, Referrers) | Beta |

Phase 2 стек: `oci-client` + `oci-spec` + наш wrapper `lofs-oci`.

### 2.3 Registry support (2026)

| Registry | Custom media types | Referrers API v1.1 | Production |
|----------|-------------------|-------------------|------------|
| **Zot** | ✅ full, без whitelist | ✅ full | Apache 2.0, vendor-neutral |
| **Harbor** | ✅ full | ✅ full | CNCF graduated |
| **CNCF Distribution** | ✅ full | ✅ full | Reference impl |
| **GHCR** | ✅ full | ✅ full | Free для OSS |
| **ACR** | ✅ (whitelisted) | ✅ full | Managed |
| **ECR** | ⚠️ whitelist-only | ✅ full | AWS-specific |
| **Quay** | ✅ full | ✅ full | Red Hat |

**Recommendation:** **Zot как primary** (vendor-neutral, ORAS test targets, нет lock-in). Harbor если нужны enterprise features.

### 2.4 Subject-reference (OCI 1.1) для SubMount

```json
{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "artifactType": "application/vnd.meteora.lofs.mergeplan.v1+json",
  "subject": {
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "digest": "sha256:abc123..."
  },
  "config": {...},
  "layers": [...]
}
```

**Use-cases:**
1. **SubMount** → subject-ref от parent snapshot к child snapshot.
2. **MergePlan** attached к target snapshot.
3. **Audit-log segment** attached к bucket manifest.
4. **Cold-tier summary** attached к consolidated snapshot-range.
5. **Signature (Ed25519/cosign)** — standard sigstore pattern, subject-ref.

### 2.5 SOCI + zstd:chunked для lazy pack pull

**SOCI zTOC** — meta-файл с per-file offset + decompressor-checkpoint. Attached как OCI artifact с subject-ref на pack. Позволяет Range-GET без full download.

**zstd:chunked** (Podman GA v5.1, 2026) — frames independently decompressible, Range-GET works. Backward-compatible zstd stream.

**Combo для нас:** **pack = zstd:chunked** + **zTOC как subject-ref artifact**. Это "best of both worlds": GA compression + lazy pull pattern.

### 2.6 Registry GC и сосhigning

- **CNCF Distribution:** mark-and-sweep GC по referenced digests.
- **Zot:** background GC с `gcDelay` (orphans > delay) + retention policies.
- **Harbor:** retention policies + GC scheduling.
- **Наш ref-count GC** (в ADR) остаётся primary; registry GC — second layer defense.

**Cosign integration:**
- Подписывает любой OCI artifact: `cosign sign --key cosign.key <registry>/.../snap@sha256:...`
- Keyless (Fulcio + OIDC + Rekor transparency log) — preferred для shareable artifacts.
- Internal Ed25519 (ADR) для per-op signing; **cosign только для publish.freeze → external-shareable snapshots**.

### 2.7 Implementation references

| Project | Что брать |
|---------|-----------|
| **wasm-to-oci** | Minimal OCI push/pull ~200 LOC Go |
| **wash CLI (wasmCloud)** | Multi-layer + metadata Rust-native |
| **Helm OCI** | Media type registration, versioning |
| **cosign attach** | Referrers API usage |
| **SOCI snapshotter** | zTOC generation + Range-GET |
| **ArgoCD OCI sources** | Consumer of custom artifacts |

### 2.8 MVP для spike

```
lofs-oci:
  push <bucket_snap> <registry>     # push manifest + layers + referrers
  pull <registry_ref> <bucket>      # fetch + materialize

Target: Zot local (`docker run zotregistry/zot`)
Validation: cosign sign + verify roundtrip
Demo: org A push → URL → org B pull, 1 TB mixed workload
```

### 2.9 Risks

- ⚠️ **ECR not friendly** к custom media types. Workaround: CNCF Distribution в EC2 вместо ECR.
- ⚠️ **`oci-client` major-version bumps** — pin в `Cargo.lock`.
- ⚠️ **Referrers API adoption uneven** у public registries. Private (Zot/Harbor) — нет проблем.
- ⚠️ **zstd:chunked не финализован в OCI spec** — implementation detail, не ABI promise.

### Ссылки Направление 2
- [OCI Image Spec v1.1](https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md) · [OCI 1.1 announcement](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/)
- [OCI Artifact Authors Guide](https://github.com/opencontainers/artifacts/blob/main/artifact-authors.md)
- [ORAS reference types](https://oras.land/docs/concepts/reftypes/) · [concepts](https://oras.land/docs/concepts/artifact/)
- [Helm OCI MediaTypes](https://helm.sh/blog/helm-oci-mediatypes/) · [HIP-0017](https://helm.sh/community/hips/hip-0017/)
- [WASM OCI (CNCF)](https://tag-runtime.cncf.io/wgs/wasm/deliverables/wasm-oci-artifact/) · [Microsoft WASM OCI](https://opensource.microsoft.com/blog/2024/09/25/distributing-webassembly-components-using-oci-registries/)
- [Rust `oci-client`](https://github.com/oras-project/rust-oci-client) · [oci-spec-rs](https://github.com/containers/oci-spec-rs)
- [Zot registry](https://github.com/project-zot/zot) · [Harbor](https://goharbor.io/blog/harbor-as-universal-oci-hub/) · [Quay Referrers](https://www.redhat.com/en/blog/announcing-open-container-initiativereferrers-api-quayio-step-towards-enhanced-security-and-compliance)
- [SOCI snapshotter](https://github.com/awslabs/soci-snapshotter) · [SOCI Parallel Pull 2025](https://aws.amazon.com/blogs/containers/introducing-seekable-oci-parallel-pull-mode-for-amazon-eks/)
- [zstd:chunked Fedora 2026](https://discussion.fedoraproject.org/t/switch-fedora-container-images-to-support-zstd-chunked-format-by-default/123712)
- [Sigstore cosign signing other types](https://docs.sigstore.dev/cosign/signing/other_types/)
- [CNCF Distribution GC](https://distribution.github.io/distribution/about/garbage-collection/)

---

## Направление 3 — Multi-agent coordination best practices

### 3.1 MAST — failure taxonomy (must-read)

**[MAST paper (Berkeley, arXiv 2503.13657)](https://arxiv.org/abs/2503.13657)** — **mandatory read**. 14 failure modes в 3 категориях, на 1600+ annotated traces из 7 frameworks, kappa=0.88:

1. **System design issues (42%)** — specification ambiguity, role misalignment, missing verification
2. **Inter-agent misalignment (37%)** — information withholding, action reneging, handoff failures, step repetition
3. **Task verification failures (21%)** — premature termination, incorrect verification signal

**Mike Mason (Jan 2026):** 57% компаний run AI agents в prod. **40% multi-agent pilots fail в 6 months. Coordination breakdown = 36.9%.**

**Для нас:** использовать MAST как **test-matrix в Phase 7 loadtest** (100 agents × 10 buckets, measure frequency каждой из 14 failure mode, validate mitigations).

### 3.2 Claim lifecycle — canonical states

```
pending → claimed → working → [blocked | awaiting_input | awaiting_approval]
                            → completed | failed | canceled | abandoned
```

- **Interrupt states** (input-required, auth-required) — temporary, task still active. Resumable через message с тем же task_id.
- **Terminal states** (completed, failed, canceled, rejected) — permanent.

**Heartbeat TTLs:**
- Temporal default: 60s heartbeat, 48h max session.
- **Для coding agents:** heartbeat каждые **30-60s, TTL 5 min** (LLM inference cycle 10-60s + network/tool call slack).

**Re-claim:**
- Stale claim → automatic release, задача в pool.
- Fork остаётся, metadata `abandoned_at` в journal.
- Другой agent видит abandoned fork → `fork.rebase` и continue.

### 3.3 Intent declaration format

```rust
pub struct IntentSpec {
    pub task_id: TaskId,
    pub goal: String,                       // 1-3 sentence NL
    pub scope: Vec<PathGlob>,               // explicit bounds
    pub estimated_duration: Duration,
    pub declared_by: AgentId,
    pub declared_at: Timestamp,
    pub blocks_on: Vec<TaskRef>,
    pub labels: BTreeMap<String, String>,   // tracker links
    pub success_criteria: Vec<String>,      // "tests pass", "merge clean"
    pub constraints: Vec<String>,           // "do not modify tests/"
    pub parent_intent: Option<TaskRef>,     // subtask hierarchy
}
```

Inspiration: A2A TaskSpec + AGENTS.md + Snap Agent Format (.agf.yaml with `constraints`) + Anthropic harness (`claude-progress.txt` + structured feature-list).

### 3.4 Discovery ergonomics

**Антипаттерн:** polling every 10s у каждого agent → quadratic load.

**Production patterns:**

1. **Pub/sub notification** при claim (Redis channel, уже есть в DevBoy) → subscribed agents получают.
2. **`intent.discover(filter)`** on-demand MCP tool: `scope_overlaps`, `active_since`, `exclude_self`.
3. **`bucket.stat(id)` warning в tool description** → **LLM читает в prompt** при работе с bucket ("⚠️ agent-xyz declared overlapping intent"). Самый ergonomic — LLM уже читает tool descriptions.

### 3.5 Partial claims / scope splitting

Три парадигмы:
- **Explicit split** (orchestrator pre-divides scope, каждый agent — disjoint slice) — **recommended default**, predictable.
- **Work-stealing** — agent claims "any remaining", pool-based. Опционально для fan-out/fan-in.
- **Scope negotiation** — 2 agents chatting через `intent.coordinate_with_peer`.

### 3.6 Cooperative intent merge / negotiation

Patterns при overlap:
1. **Priority-based:** `labels.priority` как tiebreaker (first-declared + priority).
2. **Negotiation tool:** `intent.negotiate(other_agent, proposal)` → agreed | counter | escalate_to_human.
3. **Human arbiter escalation** — если agents не договариваются > 2 rounds → flag в DevBoy UI.

**Failure mode "ADS — Agent Deadlock Syndrome":** 2+ agents взаимно defer authority, circular handoff. *Mitigation:* **max-hop 3, max 2 rounds negotiation, force human arbitration или FCFS fallback**.

### 3.7 Handoff patterns

| Pattern | Framework | Когда |
|---------|-----------|-------|
| **Explicit publish-notify** | Our fork-merge | Default |
| Temporal workflow signal | Temporal.io | Long-running (hours/days), durable |
| A2A Task transition | A2A v0.3 | Cross-org / cross-vendor |
| LangGraph Command + interrupt | LangGraph | Hierarchical supervisor + HITL |
| OpenAI Agents SDK handoff | OpenAI | Sequential delegation |
| Direct MCP tool call | Claude/rmcp | Tightly coupled 2-agent |

**Anthropic insight (90.2% improvement в multi-agent research system):** **context isolation + shared observable state**, не tight coupling. Orchestrator Opus + specialist Sonnet subagents. Естественно совпадает с fork-merge: **fork = context isolation, shared bucket = observable state**.

**Для нас:** explicit publish + notify (Anthropic-style) primary, Temporal integration optional >1h workflows, A2A interop Phase 7+.

### 3.8 Human-in-the-loop integration

**LangGraph interrupt pattern (canonical):**

```python
@interrupt_on(lambda ctx: ctx.path.starts_with("deploy/"))
def deploy_tool(...): ...
# Execution pauses, checkpointer saves state
# Resume via Command(resume=<human_decision>)
```

**GitHub Copilot Workspace pattern:** research → plan → preview → **human approve** → code changes → PR. Agent **cannot auto-merge**.

**Для lofs:**

```toml
[approval]
sensitive_paths = ["apps/**/migrations/**", "secrets/**", ".werf/**"]
on_merge = "require_human"          # для sensitive
on_commit = "auto"                   # для non-sensitive
on_delete = "require_human"          # всегда
on_publish = "audit_notify"          # email to org admins
```

Hooks:
- `merge.execute(plan)` → проверка sensitive_paths → interrupt → wait `merge.approve(merge_id, human_id, decision)`.
- `bucket.delete` → require confirmation для non-ephemeral.
- `publish.freeze` → optional audit email.

### 3.9 Public failure incidents

- **MAST 14 failure modes** — definitive catalog.
- **Anthropic multi-agent research system:** early iterations — "spawning excessive subagents for simple queries" + "redundant searches" + "poor coordination". Fix: prompt engineering для lead agent, explicit strategy in prompt, token budget per subagent.
- **Infinite handoff loops** — directive misalignment, narrow role interpretation → recursive handoff.
- **Context overload / recency bias** — long session, focus на last 3 messages, ignore goal. Митигация: compacting (Claude Code), summarization-on-compact, explicit goal re-read.
- **Coordination tax:** 4-agent pipeline ~950ms overhead vs 500ms work; 3-agent uses 29K tokens vs 10K single.
- **"Bag of agents" — 17× error trap:** accuracy saturates/fluctuates за > 4 agents без structured topology.
- **Properly orchestrated systems → 3.2× lower failure rates** than "bag of agents".

### 3.10 Deadlock / livelock

- **ADS (Agent Deadlock Syndrome)** — circular defer. Detection: time-in-state > P99, handoff count > max_hops. Mitigation: **hard max-hop 3, timeout-to-human 5min, arbiter fallback (lowest-id)**.
- **Resource-based deadlock** — classical cycle detection на resource graph. Solution: total ordering on bucket-path lock acquisition.
- **Oscillation / ping-pong** — flip decisions each round. Detection: state-change rate без progress. Mitigation: **randomized backoff, weak/strong commitment policy**.

### 3.11 Scaling beyond 4 agents

**Structural topology обязательна:**

| Topology | Scales to | Production |
|----------|-----------|------------|
| Orchestrator-worker (Anthropic) | ~20 sub-agents | Production-proven |
| Hierarchical supervisor trees (LangGraph) | ~100 agents | Production |
| Peer-to-peer mesh | Breaks ≥ 10 agents (quadratic) | Avoid |
| Message bus / blackboard (shared observable state) | Anthropic-proven | Recommended |

**Для lofs:** **1 orchestrator + N workers, N ≤ 20 per bucket**. > 20 — split в sibling buckets via sub-mount.

### 3.12 Observability

Canonical stack 2026:
- **OpenTelemetry trace** per `intent.*`, `snap.commit`, `merge.execute`.
- **Agent timeline visualization** — Grafana / Langfuse / Helicone.
- **Playback mode** — record/replay inputs/outputs per session.
- **Anomaly detection** — handoff rate outlier, token spike, state-transition stalls.

Extension для нас: **per-agent session trace linked к intent_id**.

### 3.13 Adversarial / reward hacking

- Agent скрывает intent drift (declared X, committed Y) → **dual-LLM intent-vs-outcome verify** (second LLM reads intent.goal + commits → semantic similarity, flag < threshold).
- Agent "ворует" чужой claim → **Ed25519 signatures** (ADR) + **rate limiting per-agent**.
- **Org-level policy enforcement** — `.lofs.toml` "this agent can only touch docs/, never code/".

### 3.14 MCP tools для coordination (minimum-viable)

```
# Intent lifecycle
intent.declare(bucket, IntentSpec) → intent_id
intent.start(intent_id)
intent.heartbeat(intent_id)
intent.update(intent_id, {status, progress_pct, blocker})
intent.complete(intent_id, outcome)
intent.abandon(intent_id, reason)
intent.expire(intent_id)                  // auto if no heartbeat > TTL
intent.handoff(intent_id, to_agent, context_artifact)

# Discovery
intent.discover(org, filter) → [IntentSpec]
intent.who_is_touching(bucket, path?) → [intent]

# Coordination
intent.negotiate(other_agent, proposal) → decision
intent.escalate_human(intent_id, reason)

# Verification
intent.verify_outcome(intent_id) → {intent_drift_score, discrepancies[]}
```

Storage: Postgres canonical state + Redis active heartbeats (TTL 5 min) — уже в DevBoy infra.

### 3.15 Actionable recommendations

1. **Adopt MAST taxonomy** как Phase-7 test matrix.
2. **Claim lifecycle** 7 states + heartbeat 60s / TTL 5min.
3. **Intent drift detection** via dual-LLM verify.
4. **Max hop 3, timeout 5min** для handoff chains.
5. **Structured topology** (orchestrator + ≤ 20 workers) — document как explicit antipattern "bag of agents".
6. **A2A interop** — IntentSpec schema compatible с A2A TaskCard (future-proofing).

### 3.16 Risks

- ⚠️ **Soft advisory ignored** → ADS. Mitigation: warnings в MCP tool output (LLM reads).
- ⚠️ **Intent drift undetected** — agent commits outside declared scope. Mitigation: server-side path-glob validation, rejection если не match.
- ⚠️ **Heartbeat spam** — 100 agents × 10 buckets = 1000/min. Redis handles, watch CPU. Dedup через set-union updates.
- ⚠️ **Negotiation loops** — hard limit 2 rounds.
- ⚠️ **Handoff context loss** — A's progress не materialized. Mitigation: **mandatory `intent.complete` must commit snapshot first**.

### Ссылки Направление 3
- [MAST paper](https://arxiv.org/abs/2503.13657) · [MAST GitHub](https://github.com/multi-agent-systems-failure-taxonomy/MAST)
- [Anthropic Multi-Agent Research System](https://www.anthropic.com/engineering/multi-agent-research-system) · [Effective harnesses](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents) · [Harness design](https://www.anthropic.com/engineering/harness-design-long-running-apps)
- [A2A Protocol v0.3](https://a2a-protocol.org/latest/specification/) · [Life of a Task](https://a2a-protocol.org/latest/topics/life-of-a-task/) · [GitHub](https://github.com/a2aproject/A2A)
- [LangGraph Supervisor](https://github.com/langchain-ai/langgraph-supervisor-py) · [HITL](https://docs.langchain.com/oss/python/deepagents/human-in-the-loop) · [HITL blog](https://www.langchain.com/blog/making-it-easier-to-build-human-in-the-loop-agents-with-interrupt)
- [OpenAI Agents SDK handoffs](https://openai.github.io/openai-agents-python/handoffs/) · [Orchestrating agents](https://cookbook.openai.com/examples/orchestrating_agents)
- [Temporal for AI](https://temporal.io/solutions/ai) · [Resilient agentic AI](https://temporal.io/blog/build-resilient-agentic-ai-with-temporal) · [Temporal + OpenAI SDK](https://temporal.io/blog/announcing-openai-agents-sdk-integration)
- [Cogent Orchestration Failure 2026](https://cogentinfo.com/resources/when-ai-agents-collide-multi-agent-orchestration-failure-playbook-for-2026)
- [TechAhead failure modes](https://www.techaheadcorp.com/blog/ways-multi-agent-ai-fails-in-production/) · [Galileo prevention](https://galileo.ai/blog/multi-agent-ai-failures-prevention)
- [Towards Data Science — 17x Error Trap](https://towardsdatascience.com/why-your-multi-agent-system-is-failing-escaping-the-17x-error-trap-of-the-bag-of-agents/)
- [ADS — Agent Deadlock Syndrome](https://sanjana-nambiar.github.io/news29.html) · [MDPI Deadlocks MAS](https://www.mdpi.com/1999-5903/15/3/107)
- [GitHub Copilot cloud agent 2026](https://github.blog/changelog/2026-04-01-research-plan-and-code-with-copilot-cloud-agent/)
- [AGENTS.md standard](https://anandchowdhary.com/notes/2025/agents-md-standard) · [Snap Agent Format](https://eng.snap.com/agent-format) · [Mike Mason Jan 2026](https://mikemason.ca/writing/ai-coding-agents-jan-2026/)

---

## Финальный синтез

### Top-10 must-read sources

**P1 — до Phase 2:**
1. [RustFS Lifecycle docs](https://docs.rustfs.com/features/lifecycle/) — hot+cold native в одном tool.
2. [MAST paper (arXiv 2503.13657)](https://arxiv.org/abs/2503.13657) — 14 failure modes test-matrix.
3. [Anthropic Harness design](https://www.anthropic.com/engineering/harness-design-long-running-apps) — Claude Code patterns.
4. [OCI Image Spec v1.1 Referrers](https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md) — SubMount в OCI terms.

**P2 — Phase 3-5:**
5. [ORAS reference types](https://oras.land/docs/concepts/reftypes/) — SubMount implementation.
6. [A2A Protocol v0.3](https://a2a-protocol.org/latest/specification/) — interop-ready IntentSpec.
7. [SOCI + zTOC](https://github.com/awslabs/soci-snapshotter) — lazy pull pattern.
8. [Self-hosted S3 2026](https://rilavek.com/resources/self-hosted-s3-compatible-object-storage-2026) — MinIO/Garage/SeaweedFS/RustFS state.

**P3 — watch-list:**
9. [Cogent Orchestration Failure 2026](https://cogentinfo.com/resources/when-ai-agents-collide-multi-agent-orchestration-failure-playbook-for-2026) — live incidents.
10. [Ink & Switch Keyhive](https://www.inkandswitch.com/project/keyhive/) — future E2EE multi-tenant.

### 5 concrete deltas для ADR-001

**Δ1 — Hot tier: plural (MinIO primary → RustFS когда GA).** Убрать RustFS как default, hot-tier = S3-compatible via OpenDAL. Note о alpha-state RustFS в Risks.

**Δ2 — OCI media types формализовать.** Добавить §Wire formats с таблицей `vnd.meteora.lofs.*`. Replace `publish.freeze/import` на OCI artifact push/pull (primary path).

**Δ3 — Intent + claim lifecycle formalize.** New §Intent lifecycle. 7 states + heartbeat 60s/TTL 5min + max-hop 3 + negotiation 2 rounds. 10 new intent.* MCP tools. Risks добавить ADS. MAST as test-matrix.

**Δ4 — SubMount через OCI subject-refs.** §Composition re-designed через OCI Referrers API. Cosign signatures attach через тот же механизм. Cycle detection + max-depth сохраняем.

**Δ5 — HITL hooks first-class.** New §HITL integration. `.lofs.toml` per-bucket policy. Hooks на merge/delete/publish. GitHub Copilot Workspace-pattern: agent produces, human approves. Connection к DevBoy UI (pending-approval queue).

### Phase plan impact

Was 14-16 weeks (after RESEARCH-003) → **now 16-19 weeks**:
- Phase 2 + 1-2 weeks (OCI wire format)
- Phase 3 + 2 weeks (intent lifecycle + HITL)
- Phase 5 + 1 week (OCI publish/import)

### Дополнительные spikes

- **Spike #5:** RustFS vs MinIO vs SeaweedFS на 1 TB mixed (1-2 weeks). Pick hot-tier.
- **Spike #6:** OCI push/pull roundtrip: Zot local + custom media types + cosign + referrers API (1 week).
- **Spike #7:** MAST failure injection: synthetic 10-agent scenarios, measure 14 failure modes, validate mitigations (2 weeks).

### Watch-list

- **RustFS distributed GA** (2026 H2) — revisit hot-tier.
- **MAST evolution** — test-matrix updates.
- **A2A v1.0 stabilization** — Phase 7+ A2A gateway.
- **Cosign OCI 1.1 full referrers support**.
- **zstd:chunked OCI spec finalization**.
- **Anthropic Managed Agents GA**.
- **LangGraph interrupt API stabilization**.
- **Snap Agent Format .agf.yaml adoption**.

### Резюме

Round 2 **не требует pivot'а** от fork-merge модели — ADR-001 структурно стабилен. Но:
- **RustFS преждевременно** в hot-tier — исправить на plural, MinIO primary.
- **OCI artifacts** — формализовать wire format, unlocks sharing/interop path.
- **Intent/claim lifecycle** — write out formal states, MAST как test-matrix.
- **HITL hooks** — вынести в first-class section.

Все изменения — инженерная реализация зрелых паттернов. Research-level risk не добавляется.
