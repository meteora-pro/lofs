# LOFS Implementation & Testing Plan

> Purely-Rust реализация [ADR-001](architecture/adr/ADR-001-lofs.md) + [ADR-002](architecture/adr/ADR-002-cooperative-coordination.md).
> Обновляется по мере принятия решений. **Status:** draft v4.1 (2026-04-22).

---

## TL;DR

- **Язык:** Rust 1.85+ (edition 2024), без Go-дочек runtime, без Buildah/Podman CLI-wrap.
- **Target OS:** Linux (MVP). macOS/Windows — через Linux VM (Lima/Colima/WSL2). CLI-поверхность (create/list/stat/rm) работает на macOS в single-host режиме с локальным Zot.
- **Backend:** `fuser` + `libfuse-fs` + `ocirender` + `oci-client` + `tar-rs` + `zstd` + `nix`/`caps`/`sys-mount`.
- **Registry:** **Zot local** (docker-compose) — primary dev target. **GitLab Container Registry** (`registry.meteora.pro`) — second testbed + benchmark comparison. Оба работают через единый `oci-client`.
- **Coordination:** OCI-only через intent-manifests ([ADR-002](architecture/adr/ADR-002-cooperative-coordination.md)). **Никакого обязательного Postgres / SQLite / Redis.** `RedisCoordination` и `PostgresCoordination` — опциональные extension-crates под тем же `Coordination` trait'ом, активируются флагом при запуске daemon-а.
- **MVP scope:** 4 MCP tools `lofs.{create,list,mount,unmount}`.
- **Timeline:** **~6 недель** до демо-ready L0 (Phase 0 + 1.1 + 1.2 + 1.3 + 1.4).

---

## Implementation Plan

### 1. Scope — что строим на L0

```
MCP tools (Phase 1):
  lofs.create(name, ttl_days, size_limit_mb) → bucket_id
  lofs.list(org?, filter?)                   → [BucketInfo]
  lofs.mount(bucket, mode, purpose, duration) → {mount_path, session_id} | MountError
  lofs.unmount(session, action)              → {new_snapshot_id} | {freed_bytes}
```

Mode matrix L0: `ro` (non-blocking, reads latest) · `rw` (exclusive lock) · `fork` (non-blocking, own writable branch — **commit возможен, merge в L1**).

Что **не входит** в L0: fork-merge (L1), intent API (L1+), tiered retention (L3+), AI-summaries (L6), HITL hooks (L7), CDC/chunked (L3+).

### 2. Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Agent (MCP client)                    │
└────────────────────────────┬────────────────────────────────┘
                             │ JSON-RPC over stdio / WebSocket
┌────────────────────────────▼────────────────────────────────┐
│                        lofs-daemon                           │
│                                                              │
│  rmcp server ── handlers ── orchestrator ── backend drivers  │
│      │             │                               │         │
│      ▼             ▼                               ▼         │
│   (4 tools)   Coordination                  ┌────────────┐  │
│                 trait                       │ mount: FUSE │  │
│                   │                         │ pack: tar+  │  │
│          ┌────────┴────────┐                │       zstd  │  │
│          │                 │                │ oci: client │  │
│   OciCoordination    [RedisCoord.]          └────────────┘  │
│   (default, L0)      [PgCoord.] (L1+ opt)                   │
└──────────┬──────────────────────────────────────┬───────────┘
           │                                      │
           ▼                                      ▼
       OCI Registry (Zot local / GitLab / Harbor / GHCR)
       · :latest           → HEAD snapshot manifest
       · :intent-<sid>     → active mount intents (subject → :latest)
       · :snap-<ts>        → historical snapshots
       · blobs             → tar.zst layers + config JSON
```

**Key properties:**
- Default path использует **OciCoordination**: весь state живёт в реестре, никакой внешней БД.
- **Extension backends** (Redis/Postgres) подключаются под тем же `Coordination` trait'ом для команд с требованием strong pessimistic lock или audit trail.
- **Disaster recovery:** упал daemon → поднять новый → он pull'ит `:latest` + referrers → состояние полностью восстановлено.

### 3. Crate structure

```
lofs/
├── crates/
│   ├── lofs-core/        # domain types + backend trait + stub impl
│   │   src/
│   │     lib.rs          # pub re-exports
│   │     bucket.rs       # Bucket / BucketInfo / BucketId / TTL enforcement
│   │     snapshot.rs     # Snapshot / SnapshotId / canonical encoding (via oci-spec manifest digest)
│   │     session.rs      # MountSession / session lifetime / heartbeat loop
│   │     intent.rs       # Intent manifest schema + annotations (ADR-002)
│   │     scope.rs        # PathScope (globset) + overlap detection for MountAdvisory hints
│   │     error.rs        # LofsError enum + rich MountAdvisory / PushConflict structs
│   │     policy.rs       # MountMode, UnmountAction, ConflictPolicy, SizeLimitPolicy
│   │     coord/          # Coordination trait + impls
│   │       mod.rs        # trait Coordination (async fns for acquire/refresh/release/list/gc)
│   │       oci.rs        # OciCoordination — intent manifests via Referrers API (default)
│   │       # redis.rs    # RedisCoordination — L1+ extension (feature = "coord-redis")
│   │       # postgres.rs # PostgresCoordination — L1+ extension (feature = "coord-pg")
│   │     backend/        # trait SnapshotBackend + impls
│   │       mod.rs        # trait definition
│   │       linux/        # cfg(linux) — fuser + libfuse-fs + ocirender
│   │         mount.rs    # mount/unmount via FUSE overlay
│   │         commit.rs   # upper/ → tar → zstd → blob + manifest
│   │         materialize.rs # pull image → local overlay lower/
│   │         userns.rs   # unshare + capability setup
│   │       stub.rs       # cfg(not(linux)) — returns UnsupportedPlatform
│   │     oci/            # OCI wire helpers
│   │       mod.rs
│   │       manifest.rs   # build snapshot + intent manifests, annotation vocabulary
│   │       registry.rs   # push/pull/referrers wrapper over oci-client
│   │       referrers.rs  # OCI 1.1 Referrers API client (list intents for a bucket)
│   │       # signing.rs  # Ed25519 op sign — L1+
│   │
│   ├── lofs-mcp/         # MCP server binary
│   │   src/
│   │     main.rs         # tracing subscriber + config + server start
│   │     server.rs       # rmcp::Server setup
│   │     handlers/
│   │       mod.rs
│   │       create.rs     # lofs.create
│   │       list.rs       # lofs.list
│   │       mount.rs      # lofs.mount (emits MountAdvisory on scope overlap)
│   │       unmount.rs    # lofs.unmount (emits PushConflict on overlap at commit)
│   │     advisory.rs     # MountAdvisory/PushConflict → MCP error payload formatter
│   │     config.rs       # from env + TOML (registry url, coord backend choice, TTL defaults)
│   │
│   └── lofs-cli/         # CLI binary — mirrors MCP surface + ops commands
│       src/
│         main.rs         # clap entrypoint
│         commands/       # one module per subcommand group
│           bucket.rs     # create, list, stat, rm
│           session.rs    # mount, unmount, status
│           registry.rs   # login, push, pull, benchmark (Zot vs GitLab)
│           ops.rs        # doctor, gc, daemon, skills
│
├── skills/               # Agent Skills (markdown specs per workflow pattern)
│   lofs-handoff/
│   lofs-fan-out/
│   lofs-checkpoint/
│   lofs-collective/
│   lofs-mount-discipline/
│
├── docker/               # dev environment
│   docker-compose.yml    # zot (only — no Postgres)
│   Dockerfile            # daemon image (Debian slim + fuse3)
│
├── tests/                # workspace-level integration tests (see Testing Plan)
├── benches/              # criterion benchmarks (Zot vs GitLab roundtrip)
├── docs/
├── scripts/              # dev helpers
├── Cargo.toml            # workspace
└── ...
```

### 4. Module LOC estimates

| Module | Purpose | LOC | Risk |
|---|---|---|---|
| `bucket.rs` + `snapshot.rs` + `session.rs` | Domain types, TTL, canonical hashing | 400 | low |
| `intent.rs` + `scope.rs` | Intent schema + path-glob overlap detection | 250 | low |
| `error.rs` | Error enum + `MountAdvisory` / `PushConflict` structs | 250 | low |
| `coord/oci.rs` | Default `OciCoordination` impl (intent push/pull via Referrers API + heartbeat) | 450 | med (depends on registry Referrers support) |
| `backend/trait.rs` | `SnapshotBackend` trait | 100 | low |
| `backend/linux/mount.rs` | FUSE overlay (fuser + libfuse-fs) | 700 | **high** (libfuse-fs beta) |
| `backend/linux/commit.rs` | diff upper/ → tar → zstd → blob | 500 | med |
| `backend/linux/materialize.rs` | image → overlay lower/ via ocirender | 400 | **high** (ocirender beta) |
| `backend/linux/userns.rs` | unshare + caps setup | 350 | med (platform-specific quirks) |
| `backend/stub.rs` | macOS / non-Linux graceful fallback | 80 | low |
| `oci/manifest.rs` | custom media-type manifests + annotations vocabulary | 300 | low (oci-spec handles plumbing) |
| `oci/registry.rs` | thin push/pull wrapper | 250 | low |
| `oci/referrers.rs` | Referrers API client + fallback for old registries | 200 | med (compat matrix) |
| `handlers/*` (4 tools) | MCP handler per tool | 600 | med |
| `advisory.rs` | MountAdvisory/PushConflict → MCP JSON formatter + hints[] generator | 180 | low |
| `config.rs` + `main.rs` | startup plumbing + registry URL + coord backend selection | 250 | low |
| **Total implementation** | | **~5260** | |
| Tests (see Testing Plan) | | ~3200 | |
| Docs / examples / scripts | | ~500 | |
| **Grand total Phase 1 (L0 MVP)** | | **~8960 LOC** | |

**L1+ extension crates (not included above, activated separately):**

| Crate | Layer | LOC | Status |
|---|---|---|---|
| `lofs-coord-redis` | L1+ extension | ~350 | future — `SETNX/EXPIRE` + pub/sub |
| `lofs-coord-postgres` | L1+ extension | ~450 | future — `SKIP LOCKED` + audit triggers |
| `lofs-merge` | L1 | ~2500 | future — 4-tier ladder, Mergiraf subprocess |
| `lofs-pack` | L3 | ~1500 | future — FastCDC + zstd:chunked + zTOC |

### 5. Phased milestones

```
Week 1 — Phase 0: Setup (dev env, CI, Zot docker-compose)
Week 2 — Phase 1.1: domain types + OciCoordination create/list
Week 3-4 — Phase 1.2: mount rw/ro + unmount commit/discard (FUSE + intent manifests + referrers)
Week 5 — Phase 1.3: path-scoped writes + rich MountAdvisory + heartbeat
Week 6 — Phase 1.4: hardening + Zot-vs-GitLab benchmark + MVP release (v0.1.0)
```

#### Phase 0 — Setup (Week 1)

**Цель:** работающий dev-environment, green CI, empty crate tree компилируется.

- [ ] Create `docker-compose.yml` с **только `zot`** (никакого Postgres — не нужен в MVP)
- [ ] Add `rust-toolchain.toml` (pin 1.85, edition 2024)
- [ ] CI workflow: add `cargo-nextest`, `cargo-deny` (supply chain), `cargo-audit`
- [ ] Pre-commit hooks: fmt + clippy + deny
- [ ] Validate **local Zot** OCI push/pull via `skopeo copy` smoketest (pure ops check)
- [ ] Validate **GitLab Container Registry** (`registry.meteora.pro`) tie-in (бенчмарк + production testbed)

**DoD:** `cargo test --workspace` green on CI; `make dev-up` запускает zot (single container); `docker-compose.yml` опубликован; оба registry-таргета (Zot + GitLab) доступны для integration tests.

#### Phase 1.1 — Domain + create/list (Week 2)

**Цель:** `lofs.create` и `lofs.list` работают end-to-end через OCI-реестр (никакой БД).

- [ ] `bucket.rs`, `snapshot.rs`, `session.rs` types (serde + validation)
- [ ] `intent.rs`, `scope.rs` — intent manifest schema + `globset`-based scope overlap
- [ ] `error.rs` — `LofsError` enum + `MountAdvisory`/`PushConflict` structs
- [ ] `coord/mod.rs` — `Coordination` trait с async methods
- [ ] `coord/oci.rs` — **`OciCoordination`** skeleton (только `list_mounts` через Referrers API используется в Phase 1.1)
- [ ] `oci/manifest.rs` — build snapshot + intent manifests с annotations vocabulary (`pro.meteora.lofs.*`)
- [ ] `oci/registry.rs` — push/pull + auth (Zot anonymous + GitLab bearer)
- [ ] `oci/referrers.rs` — list referrers API call + fallback to tag-scan для registry без 1.1
- [ ] Handlers: `handlers/create.rs` (push empty snapshot-manifest), `handlers/list.rs` (catalog scan + annotations parse)
- [ ] MCP server wiring (`server.rs`, `main.rs`)

**DoD:** MCP inspector показывает 2 tools; `lofs.create` push'ит empty snapshot-manifest (`:latest` tag) с annotations в **оба registry-таргета** (Zot + GitLab); `lofs.list` возвращает enriched `BucketInfo` с чтением annotations; integration test create → list roundtrip зелёный для обоих registry.

#### Phase 1.2 — mount rw/ro + unmount commit/discard (Week 3-4)

**Цель:** агент может полноценно поработать в bucket'е и закоммитить через intent-manifest coordination.

Неделя 3:
- [ ] `backend/linux/userns.rs` — unshare + user-ns + cap setup
- [ ] `backend/linux/mount.rs` — libfuse-fs integration: lower (materialized) + upper + work + merged
- [ ] `backend/linux/materialize.rs` — ocirender direct pull image → lower/
- [ ] `coord/oci.rs` — **полная реализация**: `acquire_mount` → push intent-manifest с subject → `:latest`; `refresh_mount` → repush intent с обновлённым `heartbeat_at`; `release_mount` → DELETE `:intent-<sid>` tag; `gc_stale` → list intents + filter by heartbeat.
- [ ] `handlers/mount.rs` — ro/rw path: pull `:latest` → `OciCoordination::acquire_mount` → materialize → mount → return session.
- [ ] Heartbeat background loop (spawn task per active session).
- [ ] End-to-end test: create → mount rw → write file → видно на mountpoint.

Неделя 4:
- [ ] `backend/linux/commit.rs` — upper/ diff → tar → zstd → blob.
- [ ] `handlers/unmount.rs` — commit path: pull `:latest` → compare с `base_snapshot` → append layer → push new manifest → update `:latest` → delete intent. Discard: delete intent.
- [ ] Size limit enforcement на commit (pre-push check of blob size vs `bucket.size_limit_mb`).
- [ ] Stale session GC sweeper (tokio timer ≥ `heartbeat × 3`).

**DoD:** full roundtrip Zot + GitLab: create → mount rw → write hello.txt → unmount commit → pull `:latest` → hello.txt присутствует; второй mount rw на тот же bucket **без scope** получает `MountAdvisory` с neighbours + hints (никакого hard error).

#### Phase 1.3 — path-scoped writes + rich advisory + fork (Week 5)

**Цель:** cooperative model в полной силе — scope-disjoint parallel writes + heartbeat discipline + fork mode.

- [ ] `scope.rs` — `PathScope::from_globs` + `overlap(other)` с `globset` crate.
- [ ] Scope metadata в intent-manifest annotations.
- [ ] `MountAdvisory` generator: enumerate neighbours, compute overlap, render hints.
- [ ] Fork mode: отдельный upper/ per session, не конфликтует с rw на уровне intent'а; commit = push в sibling bucket `<orig>-fork-<ts>`.
- [ ] `ack_concurrent` поле в mount args — подавляет advisory и mount'ит поверх.
- [ ] `conflict_policy` в unmount args: `reject` / `scope_merge` / `fork_on_conflict`.
- [ ] `PushConflict` payload на unmount commit когда scope overlap detected.

**DoD:** 3 mount modes зелёные в integration tests; concurrent rw на дизъюнктных scope'ах коммитят параллельно; overlap → `MountAdvisory`/`PushConflict` JSON содержит neighbour info + ≥ 2 hints.

#### Phase 1.4 — Hardening + benchmark + MVP release (Week 6)

- [ ] Observability: Prometheus metrics (tool_calls, mount_duration, commit_size, intent_gc_count)
- [ ] Tracing spans через operations (OpenTelemetry stdout exporter для dev)
- [ ] Graceful shutdown (drain sessions на SIGTERM → commit или discard по policy)
- [ ] Failure modes coverage (registry offline, registry 503, disk full, Referrers API не поддержан)
- [ ] **Benchmark: Zot vs GitLab Container Registry** — mount/unmount/list roundtrip latency, p50/p95/p99, результаты фиксируются в `bench/registry-comparison.md`
- [ ] Docs: README quickstart — works on fresh Linux box с локальным Zot, также works против GitLab registry
- [ ] Release: tag `v0.1.0`, publish crates.io (optional)

**DoD:** 100% critical tests green; quickstart в README воспроизводим от zero; benchmark baseline зафиксирован для обоих registry-таргетов; документ `bench/registry-comparison.md` опубликован.

#### Future phases (L1-L7, data-driven activation)

```
L1: Merge engine (LLM-driven, 4-tier ladder) — триггер: fork+merge workflow adoption
    + RedisCoordination extension (feature="coord-redis")      ~3-4 недели
L2: Per-file BLAKE3 dedup + reference-counting GC               ~2 недели
    + PostgresCoordination extension (feature="coord-pg")
L3: FastCDC + pack-files (zstd:chunked) + zTOC                  ~2 недели
L4: SOCI-style lazy pull + Range-GET                            ~1-2 недели
L5: Cold tier (MinIO → S3 IA → Glacier) + bookmarks             ~1 неделя
L6: AI-summaries cold tier + Ed25519 signing + embedding search  ~2 недели
L7: HITL hooks + DevBoy UI pending-approval queue                ~1 неделя
```

Активация каждого layer'а — отдельный RFC / ADR после телеметрии из предыдущего layer'а.

### 6. Rust libraries — how & where we use them

Понимать **зачем каждая зависимость в дереве** — обязательно. Ниже — семантическая матрица: что делаем, какой крейт, где именно вызывается, какие альтернативы и почему не взяли.

#### 6.1 OCI wire & registry

| Крейт | Роль у нас | Где используется | Почему именно он | Maturity |
|-------|-----------|------------------|------------------|----------|
| **`oci-spec`** | Канонические типы OCI (ImageManifest, Descriptor, MediaType, Runtime Spec) для сборки/парсинга манифестов | `crates/lofs-core/src/oci/manifest.rs` — при `lofs.create` (empty base manifest) и `lofs.unmount commit` (appending layer descriptor) | Единственный crate покрывающий все три OCI-спеки (Image/Runtime/Distribution), de-facto canonical в youki ecosystem | production |
| **`oci-client`** | HTTP push/pull к OCI-registry: pull manifest, upload blob, tag ref, bearer-auth | `crates/lofs-core/src/oci/registry.rs` — во всех 4 handlers: create (push empty), mount (pull image), unmount (push layer + new tag) | Единственный production Rust-клиент с Referrers API; используется в wasmtime/krustlet | production (pre-1.0, pin strict) |
| **`tar`** (sync tar-rs) | Упаковка overlay `upper/` в tar-архив слоя | `crates/lofs-core/src/backend/linux/commit.rs` — читает diff из upper/ и пишет canonical tar | Только sync tar — **`tokio-tar` имел CVE-2025-62518 (TARmageddon)**, все fork'и заражены | production |
| **`zstd`** | Компрессия tar-архива в tar.zst layer | `commit.rs` (сразу после tar), `materialize.rs` (decompress при pull) | Canonical Rust binding к C `libzstd`, используется в Restic/sccache | production |
| **`async-compression`** | Async wrapping zstd стримов для tokio I/O | `oci/registry.rs` — при streaming upload большого layer blob в registry | Canonical async-wrapper, поддерживает futures/tokio одним crate | production |
| **`blake3`** | Content-addressable hashing layer blob'ов + deterministic snapshot ID | `core/snapshot.rs` (SnapshotId = BLAKE3(canonical CBOR)); `commit.rs` (blob digest) | 5-10× быстрее SHA-256, industry-standard в iroh/Restic; OCI registry понимает `sha256:` — наш internal digest BLAKE3, на wire — SHA-256 через `Sha256Hasher` | production |
| **`ciborium`** | Канонический CBOR-сериализатор для snapshot (deterministic byte-level идентичность) | `core/snapshot.rs::canonical_encode()` | Default-features=false + CanonicalizationOptions даёт deterministic output; альтернатива serde_cbor не поддерживается | production |

#### 6.2 FUSE + overlay (Linux-only)

| Крейт | Роль у нас | Где используется | Почему именно он | Maturity |
|-------|-----------|------------------|------------------|----------|
| **`fuser`** | Низкоуровневый FUSE-binding (libfuse 3) — регистрация FS callbacks | Не напрямую, через `libfuse-fs` как transitive dep | Canonical FUSE crate, production в iroh-mount и других | production |
| **`libfuse-fs`** | **Готовая OverlayFS + UnionFS с whiteouts** в userspace (поверх fuser) | `crates/lofs-core/src/backend/linux/mount.rs` — setup lower + upper + work + merged layout; copy-up UID/GID handling | Единственный production-intended Rust-crate с полной OverlayFS-семантикой; альтернатива — писать самим ~1500 LOC | beta (single-maintainer) — **R1** |
| **`ocirender`** (edera-dev) | Streaming OCI layer merge → directory output с правильными whiteouts/hardlinks/PAX | `backend/linux/materialize.rs` — pull image layers → materialize в `lower/` directly (без intermediate tar-extract) | Прорывной find RESEARCH-005: экономит ~2000-3000 LOC custom layer-merge logic; производительнее `docker extract` на 31% | beta (early-adopter) — **R2** |

#### 6.3 Linux syscalls & capabilities

| Крейт | Роль у нас | Где используется | Почему именно он | Maturity |
|-------|-----------|------------------|------------------|----------|
| **`nix`** | Syscall wrappers: `unshare`, `setns`, `mount`, `umount2`, `pivot_root`, signals | `backend/linux/userns.rs` (unshare user+mount namespace); `mount.rs` (mount overlay); signal handling для graceful shutdown | Canonical Rust wrapper вокруг POSIX syscalls; используется в youki/containerd-shim-wasm | production |
| **`caps`** | Linux capability management: drop/set ambient/permitted/effective sets | `backend/linux/userns.rs` — в процессе unshare drop всё кроме нужных для mount | Единственный pure-Rust crate с полным sets-API; lucab maintainer доверен | production |
| **`sys-mount`** | High-level `mount()` builder pattern + auto-unmount guards | `backend/linux/mount.rs` — setup overlay mount (если рабочее окружение разрешает) | Удобнее чем `nix::mount` для повторяющегося pattern; RAII-guards для reliable teardown | production |

#### 6.4 Async runtime & infrastructure

| Крейт | Роль у нас | Где используется | Почему именно он | Maturity |
|-------|-----------|------------------|------------------|----------|
| **`tokio`** | Async runtime (multi-threaded) | Весь daemon: server loop, handlers, heartbeat sweeper, registry HTTP | Industry default; rmcp + oci-client все требуют tokio | production |
| **`rmcp`** | MCP server framework (JSON-RPC-over-stdio/WS) | `crates/lofs-mcp/src/server.rs` — tool registration, dispatch | Official Anthropic Rust SDK; единственный maintained MCP Rust server | production (0.2.x) |
| **`globset`** | Glob pattern matching для path scope overlap detection | `crates/lofs-core/src/scope.rs` | Fast + широко используется (ripgrep, cargo), Apache 2.0 | production |
| **`tracing`** + **`tracing-subscriber`** | Structured logging + spans | Весь workspace — инструментирование handlers + backend operations | Canonical Rust observability; JSON-output для prod | production |
| **`metrics`** + **`metrics-exporter-prometheus`** | Prometheus metrics (counters/histograms) | Daemon startup регистрирует exporter; handlers эмитят `tool_calls_total`, `mount_duration_seconds` и т.д. | Industry-standard metrics ecosystem | production |

#### 6.5 Утилиты и DX

| Крейт | Роль у нас | Где используется | Maturity |
|-------|-----------|------------------|----------|
| **`thiserror`** | Derive `Error` + `Display` на domain-errors | `core/error.rs` — LofsError enum | production |
| **`anyhow`** | Gerenic Result для binary/top-level | `lofs-mcp/src/main.rs`, CLI | production |
| **`serde`** + **`serde_json`** | Serialization для MCP payloads + config | Весь workspace | production |
| **`toml`** | Config parsing | `lofs-mcp/src/config.rs` | production |
| **`uuid`** v1 (features=`v7`) | Bucket IDs, session IDs (time-ordered) | `core/bucket.rs`, `core/session.rs` | production |
| **`chrono`** | Timestamps для TTL/expired_at | `core/bucket.rs`, `core/session.rs` | production |
| **`hex`** | Blob digest display/parse | `core/snapshot.rs`, `oci/manifest.rs` | production |
| **`clap`** v4 | CLI arg parsing | `lofs-cli/src/main.rs` | production |

#### 6.6 Что мы пишем сами (нет готового)

Эти модули — **наш code**, без прямого готового аналога. Инвестиция в ~4100 LOC.

| Модуль | Зачем свой | Примерный LOC |
|--------|-----------|---------------|
| `bucket.rs` + `snapshot.rs` + `session.rs` | Domain types LOFS — уникальная combination TTL + lifecycle + content-address | 400 |
| `intent.rs` + `scope.rs` | Intent manifest schema + glob-scope overlap detection — ADR-002 core | 250 |
| `coord/oci.rs` (`OciCoordination`) | Cooperative coordination через Referrers API — уникальная семантика, нет готового crate | 450 |
| `error.rs` (`MountAdvisory` + `PushConflict`) | Наша фича — LLM-friendly hints + neighbour info + overlap_analysis в structured errors | 250 |
| `backend/trait.rs` + `stub.rs` | Абстракция backend'а для future swap (containerd direct / WASI / другое) | 180 |
| `backend/linux/commit.rs` | Tie-it-together: inotify diff → tar_rs → zstd → oci-client push | 500 |
| `backend/linux/materialize.rs` | Thin wrapper над ocirender с fallback на plain OCI extract если ocirender edge-case не покрыт | 400 |
| `backend/linux/userns.rs` | Our unshare + caps setup — нужна точно под our rootless mount-flow | 350 |
| `oci/manifest.rs` | Custom media types (`vnd.meteora.lofs.*`) + annotations vocabulary | 300 |
| `oci/referrers.rs` | Referrers API client + fallback для registry без 1.1 поддержки | 200 |
| `handlers/*` (4 MCP tools) | Glue layer между MCP protocol и backend | 600 |
| `advisory.rs` | Hints generator + MountAdvisory/PushConflict → MCP payload | 180 |

#### 6.7 Что **не** используем в L0 (и почему)

| Не используем в L0 | Статус | Почему |
|---------------------|--------|--------|
| **`sqlx` / SQLite / Postgres** | **extension (L1+)** | L0 не требует БД — state в OCI-реестре (ADR-002). `PostgresCoordination` подключается в L2+ как feature-gated extension crate |
| **Redis** | **extension (L1+)** | Аналогично: `RedisCoordination` — L1+ extension, не mandatory runtime dep |
| **`opendal`** | **L3+** | L0 использует `oci-client` напрямую, OCI-level blob dedup достаточен. OpenDAL активируется вместе с CDC pack-layer |
| **`buildah` CLI wrap** | never | Go binary, fork/exec overhead, external runtime dep. Pure Rust — чище (см. §6.6 — пишем сами) |
| **`tokio-tar` (любой fork)** | never | **CVE-2025-62518 TARmageddon** — parser bug во всех forks (astral-sh/dignifiedquire/edera). Используем только sync `tar` + `spawn_blocking` |
| **`shiplift`, `dkregistry-rs`** | never | Abandoned с 2021. `bollard` (Docker) не нужен — мы ходим в registry, не в container runtime |
| **`containerd-client`** | never | Избыточно: мы не control-plane для контейнеров, только OCI registry клиент + overlay mount. Если L1+ потребует direct containerd snapshotter — добавим |
| **`sigstore` / `cosign-rs`** | **L2+** | Pre-1.0 experimental. Для L0 Ed25519-signing не включаем; shareable snapshots с cosign-signatures — L2+ через cosign CLI subprocess |
| **`automerge` / `loro` / `yrs`** | never | CRDT path отвергнут в ADR-001 §Alternatives — см. [RESEARCH-002](architecture/research/RESEARCH-002-crdt-fs-space.md) |
| **`fastcdc`** | **L3** | Content-defined chunking — L3 optimization. L0 plain `tar.zst` layers достаточно для MVP |

### 7. Dependencies — pinned Cargo

Pinned set per [RESEARCH-005](architecture/research/RESEARCH-005-rust-oci-ecosystem.md):

```toml
# Core OCI — production
oci-spec = "0.8"
oci-client = "=0.16.1"              # pin — pre-1.0 breaking

# FUSE / mount (Linux-only, cfg-gated)
fuser = "0.17"
libfuse-fs = "0.3"                  # beta, single-maintainer — watch

# Layer merge (beta, но прорывной)
# ocirender = { git = "https://github.com/edera-dev/ocirender", rev = "<pinned>" }
# или crates.io когда зарелизят

# Tar + zstd — production
# ВНИМАНИЕ: sync tar-rs ТОЛЬКО. Не tokio-tar (CVE-2025-62518 TARmageddon).
tar = "0.4"
zstd = "0.13"
async-compression = { version = "0.4", features = ["zstd", "tokio"] }

# Hashing
blake3 = "1.5"

# Canonical encoding (snapshot digest)
ciborium = "0.2"

# Path scope matching (ADR-002)
globset = "0.4"

# Linux syscalls (cfg-gated)
nix = { version = "0.28", features = ["mount", "sched", "user", "process"] }
caps = "0.5"
sys-mount = "3"

# Coordination — L1+ extensions (NOT in L0 MVP):
# sqlx = { version = "0.8", features = ["runtime-tokio", "tls-rustls", "postgres", "uuid", "chrono", "json", "migrate"] }
# redis = { version = "0.27", features = ["tokio-comp", "connection-manager"] }

# MCP
rmcp = "0.2"                        # when ready — current stubs without MCP

# Async
tokio = { version = "1", features = ["rt-multi-thread", "macros", "fs", "io-util", "sync", "process", "time", "signal"] }

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
metrics = "0.23"
metrics-exporter-prometheus = "0.15"

# Signing (L1+, optional)
# ed25519-dalek = "2"

# CLI (lofs-cli)
clap = { version = "4", features = ["derive", "env"] }

# Config
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Errors
thiserror = "1"
anyhow = "1"

# Utilities
hex = "0.4"
uuid = { version = "1", features = ["v7", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
```

### 8. Development environment requirements

**Host (Linux full-stack):**
- Linux 5.11+ (for unprivileged user namespaces + rootless FUSE)
- Rust 1.85+
- Zot registry (dev через docker-compose — **единственный обязательный сервис**)
- `fuse3` installed: `apt install fuse3 libfuse3-dev`
- `pkg-config`, `libssl-dev`, `build-essential`

**Optional for benchmarking / production integration:**
- Access to GitLab Container Registry (`registry.meteora.pro`) для L0 benchmark
- Redis 6+ (если экспериментируешь с `RedisCoordination` extension — L1+)
- Postgres 14+ (если экспериментируешь с `PostgresCoordination` extension — L1+)

**macOS dev:**
- Via Lima/Colima/UTM — Linux VM с Ubuntu 22.04+ для full mount-stack.
- Локально `cargo check --workspace` + `cargo test --workspace` работают (Linux-only deps cfg-gated). CLI-поверхность (create/list/stat/rm) работает на macOS напрямую против локального Zot.

### 9. Definition of Done per phase

- Phase 0: `make dev-up && cargo test --workspace && cargo nextest run --workspace` — всё зелёное
- Phase 1.x: integration tests для каждого tool в `tests/` + README-quickstart работает с чистой машины
- Phase 1.4 MVP: ни один из test-сценариев (см. Testing Plan §"Critical test scenarios") не падает; benchmark baseline published

---

## Testing Plan

### 1. Pyramid

```
                   ╱╲
                  ╱  ╲
                 ╱ E2E ╲              10-15 scenarios
                ╱   5%  ╲             docker-compose (Zot) + optional GitLab
               ╱──────────╲
              ╱            ╲
             ╱  Integration ╲         ~35 tests
            ╱       20%       ╲       testcontainers-rs (Zot only)
           ╱────────────────────╲
          ╱                      ╲
         ╱         Unit           ╲   ~150 tests
        ╱           75%             ╲ inline в каждом crate
       ╱────────────────────────────╲

Plus: property-based (merge logic — L1), fuzz (OCI parsers), perf (criterion).
```

### 2. Test frameworks + tooling

| Level | Framework | Config |
|---|---|---|
| Unit | built-in `#[test]` + `rstest` fixtures | inline в `mod tests {}` |
| Integration | `cargo-nextest` (parallel, retries) + `testcontainers-rs` | `tests/` dir; spins up Zot (только) |
| E2E | `cucumber-rs` (Gherkin BDD, как в lokb) | `tests/features/*.feature` + step defs |
| Property | `proptest` | для scope-overlap L0; merge logic в L1+ |
| Fuzz | `cargo-fuzz` (libFuzzer) | OCI manifest parser, intent annotations, error JSON |
| Coverage | `cargo-llvm-cov` | report в CI → Codecov |
| Supply chain | `cargo-deny`, `cargo-audit` | CI gate |
| Performance | `criterion` | мount/commit/list latency baseline; Zot vs GitLab comparison |

### 3. Unit test coverage by module

| Module | Key invariants | Tests |
|---|---|---|
| `bucket` | TTL math, size math, serde roundtrip, hash deterministic | ~15 |
| `snapshot` | manifest digest deterministic, parent-chain validation | ~12 |
| `session` | lifetime states (active / committed / discarded / expired) | ~10 |
| `intent` | annotations schema, roundtrip, heartbeat freshness detection | ~12 |
| `scope` | glob parsing, overlap detection (disjoint / intersecting / superset) | ~15 |
| `coord/oci` | Coordination trait contract: acquire/refresh/release/list/gc (against Zot testcontainer) | ~14 |
| `error` | `MountAdvisory` + `PushConflict` JSON shape stable, hints ordering deterministic | ~10 |
| `backend/trait` | contract traits (typestate) | ~6 |
| `backend/linux/mount` | overlay setup + teardown, idempotent | ~15 |
| `backend/linux/commit` | diff detection, whiteout handling, tar reproducibility | ~18 |
| `backend/linux/materialize` | ocirender integration — image → overlay lower/ | ~12 |
| `backend/linux/userns` | unshare + cap set/drop correctness | ~8 |
| `oci/manifest` | custom media types, annotations, subject refs | ~10 |
| `oci/registry` | push/pull auth, retry on 503, backoff | ~10 |
| `oci/referrers` | Referrers API + fallback to tag-scan compat | ~8 |
| `handlers/*` | each tool: happy path + 3 error paths | ~20 |
| `advisory` | Advisory/Conflict payload shape + hints generator determinism | ~8 |
| **Total unit** | | **~183** |

Все unit-тесты должны **не требовать FUSE / привилегий** — используем mocks/stubs. Coordination-тесты требуют Zot-testcontainer (лёгкий, ~100 MB image). FUSE-mount код тестируется в integration tier (нужен реальный kernel).

### 4. Integration tests

Dir: `tests/it_*.rs`. Каждый файл управляет testcontainer lifecycle.

| File | Scenarios | Requires |
|---|---|---|
| `it_create_list.rs` | create → list → stat → delete; TTL expiry; size limit reached | Zot |
| `it_mount_rw.rs` | mount rw → write → stat → unmount; intent-manifest lifecycle | Zot + FUSE |
| `it_mount_ro.rs` | mount ro concurrent с rw; read-only enforcement | Zot + FUSE |
| `it_mount_fork.rs` | multiple forks parallel; per-fork isolation; sibling-bucket push | Zot + FUSE |
| `it_commit_roundtrip.rs` | commit → push → pull `:latest` → content matches | Zot + FUSE |
| `it_discard.rs` | discard drops upper/; `:intent-<sid>` tag deleted; registry blobs unchanged | Zot + FUSE |
| `it_session_expiry.rs` | heartbeat stops → sweeper GC's stale intent → next mount succeeds | Zot + time mock |
| `it_oci_auth.rs` | push/pull с bearer token к GitLab testbed (optional, CI-flag gated) | live GitLab |
| `it_scope_overlap.rs` | concurrent rw с disjoint scope → both succeed; overlap → MountAdvisory | Zot + FUSE |
| `it_advisory_payload.rs` | MountAdvisory JSON содержит neighbours + overlap + ≥ 2 hints | Zot |
| `it_push_conflict.rs` | two commits race → second gets PushConflict + conflict_policy behaviour | Zot + FUSE |
| `it_size_limit.rs` | pre-commit size check; oversize reject с чистым error | Zot + FUSE |
| `it_error_paths.rs` | registry 503 retry, disk full — все error paths | toxiproxy |
| `it_race_create.rs` | parallel `lofs.create` с same name → последний wins tag, blobs preserved | Zot |
| `it_referrers_fallback.rs` | registry без Referrers API → tag-scan fallback works | mock registry |

### 5. E2E (Cucumber/Gherkin)

Dir: `tests/features/`. По примеру lokb.

```gherkin
Feature: Agent handoff via LOFS bucket
  Scenario: First agent commits, second agent reads
    Given a fresh Zot registry
    And agent A has created bucket "handoff-test" with TTL 1 day
    When agent A mounts "handoff-test" as rw with purpose "writing report"
    And agent A writes "analysis.md" with content "hello from A"
    And agent A unmounts with action commit
    Then the snapshot is pushed to the registry as :latest
    When agent B mounts "handoff-test" as ro
    Then agent B reads "analysis.md" and sees "hello from A"

  Scenario: Cooperative advisory when scopes overlap
    Given bucket "busy" is mounted rw by agent A with scope "/src/**"
    When agent B tries to mount "busy" as rw with scope "/src/auth/**"
    Then the response kind is "MountAdvisory"
    And the advisory payload contains:
      | field                          | value                              |
      | neighbours[0].agent_id         | A                                  |
      | neighbours[0].purpose          | writing report                     |
      | neighbours[0].scope[0]         | /src/**                            |
      | neighbours[0].overlap_with_req | /src/auth/**                       |
      | hints                          | includes "ack_concurrent=true"     |
      | hints                          | includes "mode=fork"               |

  Scenario: Disjoint scopes coexist without advisory
    Given bucket "parallel" is mounted rw by agent A with scope "/docs/**"
    When agent B tries to mount "parallel" as rw with scope "/src/**"
    Then the response kind is "MountSession"
    And both agents can commit independently
```

### 6. Property-based (L1+)

Когда L1 fork-merge появится:
- `proptest` генерирует пары fork-states, проверяет что:
  - merge коммутативен (по three-way diff) для непересекающихся path'ов
  - merge детерминистичен (same inputs → same output snapshot digest)
  - no file content silently dropped (inventory invariant)

### 7. Fuzz targets

```
fuzz/
  fuzz_targets/
    oci_manifest.rs    # oci-spec parse: malformed JSON
    tar_layer.rs       # pathologically-crafted tar (whiteouts, unicode paths)
    mount_error.rs     # random MountError JSON payloads — our parser
    snapshot_id.rs     # canonical-CBOR encoder idempotence
```

Запуск: `cargo fuzz run oci_manifest -- -max_total_time=60` в CI nightly.

### 8. CI matrix

```yaml
jobs:
  check:     ubuntu-latest,  stable      fmt + clippy + deny + audit
  test-unit: ubuntu-latest,  stable      cargo-nextest, unit only (--lib)
  test-it:   ubuntu-latest,  stable      nextest integration (with docker-compose)
  e2e:       ubuntu-latest,  stable      cucumber-rs + full compose
  coverage:  ubuntu-latest,  stable      cargo-llvm-cov → Codecov
  fuzz:      nightly only, scheduled     cargo-fuzz, 60s per target
  bench:     manual trigger              criterion compare vs main
```

Job-level timeouts: 10 min unit, 20 min IT, 30 min E2E.

### 9. Coverage goals

| Milestone | Unit | IT | Overall |
|---|---|---|---|
| Phase 1.1 (create/list) | ≥ 85% | ≥ 70% | ≥ 75% |
| Phase 1.2 (mount + commit) | ≥ 85% | ≥ 70% | ≥ 78% |
| Phase 1.4 (MVP) | ≥ 85% | ≥ 75% | ≥ 80% |

Exclude from coverage: generated code, test utilities, main.rs bootstrap.

### 10. Critical test scenarios (must-pass для MVP)

1. **Happy path handoff:** A creates → mount rw → write → commit → unmount → B mount ro → read matches.
2. **Scope-overlap advisory:** two rw mounts с пересекающимися scope → second gets `MountAdvisory` с neighbours + hints.
3. **Scope-disjoint parallelism:** two rw mounts с non-overlapping scope → both succeed → both commit → оба layer в `:latest`.
4. **Read-while-write:** rw held; ro mount concurrent works; ro sees **prior** snapshot (base_snapshot isolation).
5. **Discard leaves no residual:** mount rw → write → discard → no commit blob pushed → `:intent-<sid>` tag deleted → bucket size не изменился.
6. **Commit of empty overlay:** mount rw → nothing written → unmount commit → no-op (no new snap).
7. **Stale-intent GC:** mount rw → kill heartbeat → `heartbeat_at + threshold` старше now → следующий mount GC'ит мёртвый intent и успешно монтируется.
8. **Size limit:** commit would exceed → reject с `SizeLimitExceeded` + hint.
9. **TTL expiry of bucket:** create bucket TTL=1d → fast-forward clock → bucket flipped to `expired` → list excludes by default, `--all` включает.
10. **Registry outage during commit:** simulated 503 → retry с exponential backoff → success eventually.
11. **Concurrent `:latest` race:** two commits race → tag update retry с `If-Match`; один win'ит, blob другого сохранён в registry (recoverable).
12. **Fork mount non-blocking:** rw held; `mode=fork` mount succeeds instantly; fork can commit как sibling bucket `<orig>-fork-<ts>`.
13. **Rich advisory correctness:** `MountAdvisory` JSON includes `neighbours[].purpose`, `neighbours[].expected_until`, `overlap_with_request`, ≥ 2 `hints[]`.
14. **OCI wire format:** push manifest → pull manifest через `skopeo inspect` → media types (`vnd.meteora.lofs.snapshot.v1+json` + `.intent.v1+json`) правильные.
15. **Startup / shutdown:** SIGTERM → graceful drain active sessions → commit или discard per policy → clean intent deletion → exit 0.
16. **Referrers API fallback:** mock registry без Referrers API → tag-scan fallback отдаёт тот же set active intents.
17. **Zot vs GitLab parity:** test suite прогоняется против обоих registry-targets, identical results (fuck-all divergence на core-flow).

### 11. Test fixtures

```
tests/fixtures/
  buckets/            # seed bucket states for parametric tests
  manifests/          # golden OCI manifests for roundtrip
  overlays/           # pre-recorded upper/ states (tar snapshots)
  agents/             # mock agent sessions with predefined purposes
  errors/             # golden JSON payloads of each error variant
```

Fixtures генерируются via helper binary `cargo run -p lofs-core --example gen-fixtures`.

### 12. Non-functional tests

| Metric | Target | Benchmark name |
|---|---|---|
| `lofs.create` latency (Zot local) | p50 < 50ms, p99 < 200ms | `bench_create_zot` |
| `lofs.create` latency (GitLab remote) | p50 < 300ms, p99 < 1s | `bench_create_gitlab` |
| `lofs.mount rw` cold (image pull) | p50 < 500ms (empty), < 5s (10MB image) | `bench_mount_cold` |
| `lofs.mount rw` warm (cached) | p50 < 100ms | `bench_mount_warm` |
| `lofs.unmount commit` (1 MB diff) | p50 < 300ms | `bench_commit_1mb` |
| `lofs.unmount commit` (100 MB diff) | p50 < 3s | `bench_commit_100mb` |
| `lofs.list` 1000 buckets (Zot) | p50 < 100ms | `bench_list_zot_1k` |
| `lofs.list` 1000 buckets (GitLab) | p50 < 2s | `bench_list_gitlab_1k` |
| Concurrent 10 scope-disjoint mounts/sec | no races, all succeed | `bench_concurrent_scope_disjoint` |
| Intent refresh latency | p50 < 50ms | `bench_intent_refresh` |
| Referrers API lookup | p50 < 50ms | `bench_referrers` |
| Memory footprint daemon idle | < 40 MB RSS | `bench_daemon_footprint` |

Benchmarks — через `criterion`. Публикуются в `target/criterion/` + сравниваются с main в CI.

**Registry comparison report** (обязательный artifact MVP v0.1.0): `bench/registry-comparison.md` с p50/p95/p99 latency для **Zot local** vs **GitLab Container Registry** на всех benchmark scenarios выше — для выбора targetregistry per deployment profile.

### 13. Test data management

- **Registry seeds:** предзагруженные manifests + blobs в Zot testcontainer через `docker exec zot oras push`, без SQL.
- **Zot testbed:** предзагруженные images для mount cold-start scenarios.
- **GitLab testbed:** опциональный CI job (`bench-gitlab`), запускается manual trigger или nightly — нужен CI secret с service-account token.
- **Deterministic time:** `tokio::time::pause()` + `advance()` для TTL / heartbeat тестов.
- **Deterministic hashes:** все `uuid::Uuid::new_v7()` → тестовый fixed-seed генератор в test builds.

### 14. Test automation gates

**Pre-commit (local):**
- `cargo fmt --check`
- `cargo clippy --workspace -- -D warnings`
- unit tests only (fast)

**Pre-merge (GitHub Actions):**
- full unit + integration + E2E
- coverage ≥ targets из §9
- `cargo deny check`
- benchmark не регрессирует > 15%

**Nightly:**
- Fuzz (60s per target)
- E2E full matrix (L0 + L1 when ready)
- Long-soak: 10k random operations без race/leak

---

## Risks & Mitigations (integrated)

| # | Risk | Impact | Mitigation |
|---|------|--------|-----------|
| R1 | `libfuse-fs` beta: breaking API / single maintainer | Phase 1.2 blocker | Pin strict version; abstract за traitом; готовность fork'нуть crate если нужно; fallback на `fuser` + свой overlay-manager (~1500 LOC додаток) |
| R2 | `ocirender` beta: неполные whiteouts / hardlinks edge-cases | Materialize fails | Pin rev; поверх него собственный wrapper с fall-through к plain OCI extract если edge-case не покрыт |
| R3 | Rootless FUSE требует user namespaces enabled (kernel 5.11+) | Platform incompatibility | Startup-check userns availability → clean "unsupported kernel" error; доки указывают минимальное ядро |
| R4 | `oci-client` pre-1.0 breaking changes | Integration test drift | Pin `=0.16.1` exact; track upstream; abstract за adapter trait |
| R5 | OCI Referrers API не у всех registry; eventual consistency | Neighbours invisible briefly → race при concurrent rw | Tag-scan fallback в `oci/referrers.rs`; pull-before-commit делает повторный check; conflict_policy fallback + benchmark Zot vs GitLab quantify задержку |
| R6 | GitLab registry API quirks (auth expiry, rate limits) | Integration flake | Explicit retry policy + observability на 429/401; tests с toxiproxy |
| R7 | Test harness flakiness (Docker-in-Docker, testcontainers cleanup) | CI red herrings | `nextest --retries=2`; dedicated cleanup hooks; hermeticity fixtures |
| R8 | Observable growing scope creep (fork+merge drifting into L0) | Timeline slip | Hard cap L0 = 4 tools; fork mode L0 creates sibling bucket only (no merge); merge строго L1 |
| R9 | macOS dev experience painful (Linux-only deps) | Developer friction | Lima config + docs; CI Linux-only; macOS cargo check via cfg-gating работает; CLI-поверхность работает на macOS с local Zot |
| R10 | Registry image size growth (without chunking) | Cost / speed | L0 plain zstd layers; chunking — L3 trigger; document cost model |
| R11 | Last-writer-wins на tag `:latest` при concurrent commits → loss of one commit tag | Data integrity concern | `If-Match` ETag в PUT tag когда registry поддерживает; blobs сохраняются в любом случае → recoverable; conflict_policy=fork_on_conflict для paranoid workflows |
| R12 | Agent игнорирует `MountAdvisory` и передаёт `ack_concurrent=true` для всех mount'ов | Silent data races | Server-side path-glob enforcement на commit (в L1+ с PostgresCoordination); в L0 — audit log через tracing + dashboard alert |

---

## Review checklist (перед MVP v0.1.0)

- [ ] All critical scenarios (§10) зелёные
- [ ] Coverage targets (§9) достигнуты
- [ ] Benchmarks (§12) в baseline
- [ ] Security review: Ed25519 signing интегрирован минимально (даже если optional в L0)
- [ ] `cargo deny check` zero findings
- [ ] README quickstart работает на clean Ubuntu 22.04
- [ ] ADR-001 обновлён если дизайн эволюционировал
- [ ] CHANGELOG.md созданный, v0.1.0 запись
- [ ] GitHub release с prebuilt binaries (Linux x86_64 + aarch64)
- [ ] crates.io publish (optional — может быть позже)

---

## Приложение: open questions до start

1. **MCP transport** — stdio only для dev, WebSocket для prod? Или оба сразу? (rmcp поддерживает оба.)
2. **Scope notation** — gitignore-style glob (`/src/**/*.ts`) или prefix-only (`/src/auth/`)? gitignore-style гибче, но требует `globset`; prefix проще для первой версии. Рекомендую `globset` с prefix-shorthand как sugar.
3. **Heartbeat interval / stale threshold** — default heartbeat 30 сек, stale 90 сек? Configurable via `daemon.toml`? Benchmark покажет trade-off (чаще heartbeat = больше HTTP, но меньше stale window).
4. **Session path** — где материализовать overlay: `/var/lib/lofs/sessions/<id>` (system-wide) или `$HOME/.local/share/lofs/sessions/<id>` (per-user)? Для rootless второе проще.
5. **Bucket naming** — globally unique per org, или allow slash-separated paths (`org/team/bucket`)? OCI repository path естественно поддерживает нестинг (`lofs/<org>/<team>/<bucket>`).
6. **Fork commit policy L0** — создавать sibling bucket автоматически (`orig-name-fork-{ts}`), или требовать явное `--as-bucket <new-name>`?
7. **ETag / `If-Match` support matrix** — какие registry поддерживают conditional tag PUT? Нужен smoketest в Phase 0 для Zot + GitLab + Harbor + GHCR.
8. **Custom media types acceptance** — все target registry принимают `vnd.meteora.lofs.*` media types? Phase 0 smoketest обязателен.

Эти вопросы не блокируют Phase 0-1.1, но на них нужны ответы до Phase 1.2. Можем решить по ходу или отдельным обсуждением.
