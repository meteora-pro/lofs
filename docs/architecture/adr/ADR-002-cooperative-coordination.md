---
id: ADR-002
title: Cooperative Coordination Model (OCI-only intent manifests)
status: proposed
date: 2026-04-22
deciders: ["Andrey Maznyak"]
tags: ["lofs", "coordination", "oci", "referrers", "intent", "multi-agent", "extension"]
supersedes: null
superseded_by: null
related_goals: []
related_issues: []
---

# ADR-002: Cooperative Coordination Model

## Status

**proposed** — дополняет [ADR-001](ADR-001-lofs.md). Фиксирует, как LOFS-агенты координируются между собой **без внешней БД** в качестве обязательного компонента.

## Context

[ADR-001](ADR-001-lofs.md) говорит «Metadata KV: PostgreSQL (DevBoy)» и предполагает pessimistic lock через Postgres `FOR UPDATE SKIP LOCKED` для mount-координации. Это воспроизводит архитектуру Harbor / Quay / GitLab Container Registry: registry для content + SQL для metadata.

При переосмыслении минимального жизнеспособного ядра стало ясно:

1. **Для агентских workflow'ов** (handoff, fan-out, checkpoint) жёсткий pessimistic lock часто избыточен. Два агента, редактирующие разные поддиректории одного бакета, не конфликтуют в реальности — но Postgres-лок их сериализует.
2. **OCI-реестр уже даёт** то, что нужно для координации: тэги как mutable pointers, manifests с annotations, [Referrers API](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#referrers-api) (OCI 1.1) для прикрепления эфемерных artifact'ов к основному снимку.
3. **Зависимость от внешней БД** повышает operational overhead (Docker-compose с Postgres, миграции, connection pools), снижает recovery story (потеря БД = потеря identity бакетов, даже если контент в реестре цел).
4. **Команды, которым реально нужен pessimistic lock** (50+ агентов, конкурентные rw на одни и те же пути каждые секунды), — меньшинство. Для них должен быть чистый plug-in, а не требование к MVP.

### Use-cases, которые должны работать без внешней БД

- Solo-developer + 1-2 агента на одной машине.
- Team of 5-10 agents на общем GitLab registry, без выделенного Postgres.
- Transient CI job в Kubernetes, который создаёт bucket, делает работу, коммитит, удаляет себя.
- Disaster recovery: реестр жив, БД умерла — всё ещё можем восстановить identity бакетов.

### Use-cases, где нужен strong lock

- 50+ агентов, пишущих в один «hot» bucket каждые секунды.
- Jobs, где silent concurrent write абсолютно недопустим (платёжные данные, audit).
- Deployments, где SLA требует detectable coordination failure в пределах миллисекунд.

## Decision

> **OCI-реестр — единственный обязательный бэкенд координации.** Состояние бакета (identity, текущий HEAD, активные mount-intent'ы) представлено как набор OCI-манифестов под одной repository path. Координация агентов cooperative: intent-декларации + pull-before-write + scope-awareness. SQL/Redis подключаются как опциональные extension-backends через trait `Coordination`, и только когда telemetry показывает потребность в strong guarantee.

### Ключевые принципы

1. **OCI-реестр как source of truth** — identity, content и координация живут в одном месте.
2. **Cooperative, не pessimistic** — `mount rw` не блокирует, а **информирует**. Решение о продолжении / отступлении / fork — у агента.
3. **Path-scoped writes** — агент декларирует `scope` (glob-паттерны). Дизъюнктные scope'ы коммитятся параллельно без конфликта.
4. **Git-style commit flow** — `unmount commit` = `pull latest → diff against base → push new layer`. Конфликт = legitimate concern для LLM, а не административный lock.
5. **Extension-ready** — `Coordination` trait покрывает все точки: default impl — OCI-only, альтернативные — Redis / Postgres / custom.

### Bucket storage layout в OCI

Один bucket = одна OCI repository:

```
registry.example/lofs/<org>/<bucket>
├── :latest                  → текущий HEAD snapshot-manifest
├── :snap-<ts>               → исторические snapshot'ы (при каждом commit)
├── :intent-<session_id>     → эфемерные intent-манифесты активных rw-сессий
│                              (subject → :latest → автоматически видны через Referrers API)
└── blobs                    → слои (tar.zst), config JSON, intent JSON
```

**Снимок** = OCI image-manifest с media type `application/vnd.meteora.lofs.snapshot.v1+json`, annotations описывают bucket identity и parent snapshot digest.

**Intent-манифест** = OCI image-manifest с media type `application/vnd.meteora.lofs.intent.v1+json`, `subject` указывает на текущий `:latest`, в `annotations` — все свойства живой сессии.

### Intent manifest schema

```json
{
  "schemaVersion": 2,
  "mediaType": "application/vnd.meteora.lofs.intent.v1+json",
  "subject": {
    "mediaType": "application/vnd.meteora.lofs.snapshot.v1+json",
    "digest": "sha256:<current latest snapshot digest>",
    "size": 1234
  },
  "config": {
    "mediaType": "application/vnd.meteora.lofs.intent.config.v1+json",
    "digest": "sha256:<config blob holding extended details>",
    "size": 512
  },
  "layers": [],
  "annotations": {
    "pro.meteora.lofs.kind": "intent",
    "pro.meteora.lofs.session_id": "01JHX...",
    "pro.meteora.lofs.agent_id": "claude-A",
    "pro.meteora.lofs.mode": "rw",
    "pro.meteora.lofs.purpose": "refactor oauth module",
    "pro.meteora.lofs.scope": "/src/auth/**,/tests/auth/**",
    "pro.meteora.lofs.started_at": "2026-04-22T20:51:06Z",
    "pro.meteora.lofs.expected_until": "2026-04-22T21:21:06Z",
    "pro.meteora.lofs.heartbeat_at": "2026-04-22T20:52:06Z",
    "pro.meteora.lofs.base_snapshot": "sha256:<digest captured at mount time>"
  }
}
```

Все поля имеют value-level значение: `agent_id` для human-readable discovery, `scope` для overlap-анализа, `heartbeat_at` для stale-detection, `base_snapshot` для определения fast-forward vs rebase.

### Mount flow (rw mode)

```
Agent A:
  1. PULL  /v2/<bucket>/manifests/latest                 → head_digest
  2. GET   /v2/<bucket>/referrers/<head_digest>          → список referrer-manifestов
  3. filter kind=intent AND heartbeat_at > now - stale_threshold
  4. Decision by Coordination trait:
      a) Нет активных intent'ов
           → PUSH свой intent-manifest с subject → head_digest
           → materialize overlay (ocirender pulls layers)
           → return { session_id, mount_path }
      b) Есть intent-ы, все scope'ы дизъюнктны с моим
           → PUSH intent + mount + return (как (a))
      c) Есть intent(ы) с пересекающимся scope
           → НЕ push-им свой intent
           → return MountAdvisory {
               neighbours: [{agent_id, purpose, scope, expected_until, session_id}],
               hints: [
                 "wait ~N seconds (longest expected_until)",
                 "narrow scope to exclude /src/auth/**",
                 "mount --mode fork to branch off",
                 "retry with ack_concurrent=true to proceed despite overlap",
               ],
             }
  5. Background heartbeat: каждые H секунд repush intent с новым heartbeat_at
```

**Intent TTL:** `heartbeat_at + stale_threshold`. Если следующий pull видит intent старше этого — он игнорируется (и может быть GC'нут любым агентом на следующем коммите).

### Unmount flow (commit)

```
Agent A (session_id s, base_snapshot B, scope S):
  1. PULL  /v2/<bucket>/manifests/latest → current_head C
  2. Diff: если C == B → fast-forward path
          если C != B → кто-то коммитил после меня:
              - pull snapshot C, посмотреть его annotations:
                  layer_scope = C.annotations[..scope] (если автор тоже объявлял scope)
              - если my_scope ∩ layer_scope == ∅ → не пересекаются, safe to append
              - иначе → PushConflict, см. conflict_policy
  3. Pack overlay upper/ → tar.zst blob → PUSH /v2/<bucket>/blobs/uploads
  4. Build new snapshot-manifest:
         parent = C  (not B — мы принимаем, что между B и C были чужие коммиты)
         layers = C.layers + [new_layer_blob]
         annotations[..scope] = S
         annotations[..author] = agent_id A
         annotations[..session_id] = s
  5. PUSH new manifest; retry PUT :latest with If-Match=<current_latest_etag> если registry поддерживает
     иначе — unconditional PUT (последний writer win'ит tag, но blob/manifest остались целы)
  6. PUSH :snap-<ts> тегом на этот же digest для историии
  7. DELETE :intent-<session_id>
```

**Conflict policies** (client-side, передаются в `unmount` tool args):

- `reject` (default) — любое пересечение scope → отказ коммита, агент получает `PushConflict` со списком изменившихся путей и решает что делать.
- `scope_merge` — если пересечение в *разных файлах* внутри scope → overlay накладывается; если в одних и тех же файлах → reject.
- `fork_on_conflict` — при конфликте автоматически push snapshot в новый sibling bucket `<orig>-fork-<ts>`, не трогая `:latest` исходного.

### Path-scoped writes — ключевая фича

Традиционный mount-lock: одна `rw`-сессия = весь bucket заблокирован.

В LOFS:

- Agent A: `mount rw --scope="/src/auth/**"` — пишет в auth.
- Agent B: `mount rw --scope="/docs/**"` — параллельно пишет в docs.
- Agent C: `mount rw --scope="/tests/integration/**"` — параллельно в tests.

Все три intent'а видны всем через Referrers API. Все три коммитят независимо. Конфликт возможен только при реальном пересечении (например, B расширил scope и начал писать в `/src/auth/`) — в этот момент срабатывает commit-time check.

**Scope notation** — glob-паттерны (syntax: gitignore / `globset` crate). Default — `**` (весь bucket) для обратной совместимости. Рекомендация best-practice — всегда указывать минимальный scope (пропагандируется в skill `lofs-mount-discipline`).

### Heartbeat и stale-intent GC

- Агент публикует intent при mount + обновляет его каждые `H` секунд (default 30).
- `stale_threshold` = `H × 3` (default 90 сек) — intent старше этого считается мёртвым.
- При mount любой агент может уничтожить `:intent-<session_id>` тег если его `heartbeat_at > stale_threshold`.
- При commit любой агент GC'ит stale-intent'ы мимоходом.
- Опциональный background sweeper daemon-а добавляет deterministic cleanup.

### Coordination trait — extension surface

```rust
// crates/lofs-core/src/coord/mod.rs
pub trait Coordination: Send + Sync {
    /// Called by `lofs.mount`. Returns either a granted session or an advisory.
    async fn acquire_mount(
        &self,
        bucket: &Bucket,
        request: MountRequest,
    ) -> LofsResult<MountDecision>;

    /// Repushes intent with fresh heartbeat_at. Called periodically while session alive.
    async fn refresh_mount(&self, session_id: &SessionId) -> LofsResult<()>;

    /// Drops the intent. Called by `unmount commit` / `unmount discard`.
    async fn release_mount(&self, session_id: &SessionId) -> LofsResult<()>;

    /// Inspects all active intents attached to bucket's :latest.
    async fn list_mounts(&self, bucket: &Bucket) -> LofsResult<Vec<ActiveIntent>>;

    /// GCs stale intents. Returns count collected.
    async fn gc_stale(&self, bucket: &Bucket, now: DateTime<Utc>)
        -> LofsResult<u64>;
}
```

**Provided implementations:**

| Impl | Когда использовать | Trade-offs |
|------|-------------------|-----------|
| **`OciCoordination`** (default) | Solo / small team (< ~20 concurrent rw), zero infra | No strong lock. Race window = HTTP-round-trip ~50ms. Scope-disjoint работает. Full concurrent rw на один файл → последний commit побеждает по tag (но blob сохранён). |
| **`RedisCoordination`** | Medium team с одним daemon-кластером, нужен strict heartbeat | `SET NX EX` для lock, `EXPIRE` для TTL, pub/sub для real-time neighbour notifications. Требует Redis 6+. |
| **`PostgresCoordination`** | Large team, shared DevBoy infra, compliance-требует audit-trail | `FOR UPDATE SKIP LOCKED` для mount queue, SQL-триггеры для audit, изоляция transactions. Требует Postgres 14+. |

Все три impl'а обязаны **публиковать intent в OCI** тоже, даже когда используют strong backend — это держит registry валидным source-of-truth и позволяет debug/recovery. Strong backend — дополнительный authoritative слой, не замена OCI.

### Rich `MountAdvisory` payload

Когда координация говорит «подожди / fork / ack», агент получает **structured JSON**, который можно положить в LLM context без потерь:

```json
{
  "kind": "MountAdvisory",
  "bucket": "dev-666-research",
  "requested_mode": "rw",
  "requested_scope": ["/src/**"],
  "neighbours": [
    {
      "session_id": "01JHX...",
      "agent_id": "claude-B",
      "mode": "rw",
      "purpose": "update oauth middleware",
      "scope": ["/src/auth/**"],
      "started_at": "2026-04-22T20:51:06Z",
      "expected_until": "2026-04-22T21:21:06Z",
      "heartbeat_age_sec": 12,
      "overlap_with_request": ["/src/auth/**"]
    }
  ],
  "hints": [
    "neighbour claude-B should release by 21:21:06 (in ~29 min)",
    "narrow your scope to /src/billing/** to proceed immediately",
    "retry with ack_concurrent=true to accept scope overlap (last-writer-wins on conflicting files at commit time)",
    "use mode=fork to branch off without contending for :latest"
  ]
}
```

Это **read-to-decide** формат: LLM агента видит всё нужное для interpretive action, не нужно дополнительных tool calls.

## Consequences

### Positive

- ✅ **Zero external infrastructure** для MVP — только OCI-реестр.
- ✅ **Single source of truth** — state в registry не расходится с БД.
- ✅ **Disaster recovery** — даже катастрофическая потеря daemon-state восстанавливается из registry.
- ✅ **Path-scoped parallelism** — N агентов в дизъюнктных scope'ах работают параллельно без contention.
- ✅ **Git-mental-model** — agents-как-committers, pull-before-push, конфликт — это сигнал, а не исключение.
- ✅ **Extension-first** — Redis/Postgres подключаются без рефакторинга core'а.
- ✅ **Cheaper unit tests** — нет testcontainers Postgres, только Zot testbed.
- ✅ **Friendly for CI/ephemeral deployments** — kubernetes job без stateful dependencies.

### Negative

- ❌ **No strong pessimistic lock в default-path** — concurrent `rw` на одни и те же файлы разрешаются на commit-time, не на mount-time.
- ❌ **Bigger attack surface на LLM reasoning** — агенту нужно понять `MountAdvisory` и принять решение, а не просто получить exception.
- ❌ **Eventual consistency registry** — редкие корнер-кейсы где intent push виден не сразу всем (особенно cross-region registry-cluster).
- ❌ **`lofs list` дороже** — полный catalog scan + per-repo manifest pull. Для <1000 buckets/org приемлемо, для большего объёма нужен cache или strong backend.
- ❌ **Rate limits** — Docker Hub / GHCR имеют pull-rate-limits. Production deploy требует самохостинг Zot / Harbor.

### Risks

| # | Risk | Mitigation |
|---|------|-----------|
| R1 | Registry eventual consistency → intent не виден concurrent агенту | Overlap detection на commit-time (не только mount-time); conflict_policy определяет fallback; benchmark с реальным GitLab регистром для quantify задержки |
| R2 | Race между PUSH intent и PULL latest (T0C) | По дизайну принимается: pull-before-commit снова проверяет intent'ы. Agent'у возвращается MountAdvisory, который содержит full context для re-decide |
| R3 | Мёртвые intent'ы накапливаются | Heartbeat + stale_threshold + GC on-commit + optional background sweeper |
| R4 | Agent игнорирует advisory и коммитит поверх всего | Server-side path-glob enforcement на commit (только у extension backends); в OCI-only mode — last-writer-wins на уровне tag, но blob retention позволяет recover |
| R5 | Registry не поддерживает `If-Match` для conditional tag update | Ключевое mitigation — Referrers API (OCI 1.1), который стандартен. Для old registry — degraded fallback через timestamp comparison в annotation |
| R6 | Custom media types не все registry принимают | Test matrix: Zot ✅, Harbor ✅, GHCR ✅, GitLab ✅, Docker Hub частично. Distribution-spec 1.1 требует поддержку unknown media types |

## Alternatives

### Alt 1: Postgres-first (supserseded)

Изначальная позиция ADR-001. Rejected для MVP из-за operational overhead и отсутствия стоящего benefit для solo/small-team use-cases. **Доступен как `PostgresCoordination` extension.**

### Alt 2: Redis-first

Redis легче Postgres (один контейнер, память), но всё ещё external dep. Rejected как default, но **доступен как `RedisCoordination` extension** для команд с существующей Redis инфрой.

### Alt 3: etcd / ZooKeeper consensus

Industrial-strength coordination, но overkill для agent-scale workloads и добавляет significant operational burden. **Not planned** — если когда-нибудь понадобится, это будет отдельный `EtcdCoordination` extension.

### Alt 4: FoundationDB / TiKV distributed KV

Scale beyond SMB, но unreasonable для LOFS first customers. **Not planned** — на таком scale пользователь перерастёт LOFS и уйдёт на in-house coordination.

## Implementation plan

Mapping на [IMPLEMENTATION_PLAN.md](../../IMPLEMENTATION_PLAN.md):

- **Phase 1.1** — `OciCoordination` basic skeleton (поддержка create/list через registry manifest + annotations).
- **Phase 1.2** — `mount/unmount` через intent manifests; heartbeat loop; pull-before-commit path.
- **Phase 1.3** — path-scoped writes enforcement; rich `MountAdvisory`; scope-overlap detection.
- **Phase 1.4** — benchmark Zot vs GitLab registry на repeated mount/unmount; document degraded-path behaviour для registry без Referrers/If-Match.
- **Phase 2+ (extension)** — `RedisCoordination` implementation под тем же trait'ом. Enabled через `--coord-backend redis` CLI flag или config.

## References

### OCI
- [OCI Distribution Spec 1.1 — Referrers API](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#referrers-api)
- [OCI Image Spec 1.1 — subject field](https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md#image-manifest-property-descriptions)
- [ORAS reftypes](https://oras.land/docs/concepts/reftypes/)
- [Zot registry](https://github.com/project-zot/zot)

### Cooperative models
- [Git merge strategies](https://git-scm.com/docs/merge-strategies) — mental model reference
- [Jujutsu working-copy-as-commit](https://martinvonz.github.io/jj/latest/working-copy/)
- [Harbor coordination architecture](https://goharbor.io/docs/2.10.0/install-config/)

### LOFS-internal
- [ADR-001: LOFS design](ADR-001-lofs.md)
- [RESEARCH-004: RustFS+S3 + OCI coordination](../research/RESEARCH-004-rustfs-oci-coordination.md) — historical, OCI-only pivot superseded SQL path
- [RESEARCH-005: Rust OCI ecosystem](../research/RESEARCH-005-rust-oci-ecosystem.md) — library selection

---

## Changelog

| Дата | Автор | Изменение |
|------|-------|-----------|
| 2026-04-22 | Andrey Maznyak | v1 — initial draft. Split out from ADR-001 to capture the shift from pessimistic SQL locks to OCI-only cooperative coordination as a first-class architectural decision. |
