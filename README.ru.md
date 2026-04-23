# lofs — Layered Overlay File System для AI-агентов

[English](README.md) | [Русский](README.ru.md)

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-concept-orange.svg)](#статус)

**LOFS** — легковесный rootless-бэкенд для **shared workspace** агентов AI, поверх OCI-реестров. Даёт агентам обычную POSIX-файловую систему, которую можно смонтировать, работать в ней и зафиксировать изменения как immutable-слой — с поддержкой fork/merge, ephemeral TTL и cooperative-разрешения конфликтов между конкурентно работающими агентами.

Можно думать о нём так: **`git worktree` + `docker image layer` + `object storage` — но специально под handoff, fan-out/fan-in и checkpoint-паттерны, которые нужны multi-agent системам.** Агент видит плоскую файловую систему (`/mnt/lofs/<session>`), а внутри каждый коммит — это OCI-слой, который пушится в любой Docker-compatible реестр (Zot, Harbor, GHCR).

У LOFS **нет обязательной БД**. OCI-реестр — единственный источник истины: и контент (слои), и координационное состояние (intent-манифесты, прикреплённые через OCI Referrers API). Опциональные Redis/Postgres-бэкенды для команд, которые переросли cooperative-модель, подключаются через один trait `Coordination`.

> **Статус: концепция / скелет инфраструктуры.** Пока неиспользуемо. Дизайн-документы — в [`docs/architecture/adr/`](docs/architecture/adr/). Первая рабочая сборка: L0 (`lofs.create / list / mount / unmount`) — цель 2026-Q2.

## Семейство: LOKB ↔ LOFS

LOFS парный продукт к **[LOKB](https://github.com/meteora-pro/lokb)** — Local Offline Knowledge Base. Они закрывают два комплементарных слоя состояния агента:

| | **LOKB** | **LOFS** |
|---|---|---|
| **Что хранит** | Постоянные знания (заметки, факты, прошлые работы) | Эфемерный рабочий контекст (задачи, артефакты) |
| **Время жизни** | Годы | Часы – дни – недели |
| **Сеть** | Offline-first | **Offline-capable** — локальный OCI-реестр (Zot на localhost) полностью покрывает single-host сценарий; remote sync опционален, когда нужно делиться между машинами |
| **Структура** | Tantivy FTS + векторные эмбеддинги + графы | Файлы + content-addressable snapshot'ы + fork'и |
| **Git-аналог** | Obsidian / zettelkasten | Git worktree + stash |

Цикл агента: `lokb.search(...)` → «что я знаю» → `lofs.mount(rw, ...)` → «работаю над этим» → `lofs.unmount(commit)` → опционально `lokb.import(...)` → «запомнил вывод».

## Зачем это нужно

Зрелым AI-агентам нужен shared-state primitive, который:

- **Эфемерен** — bucket с явным TTL, а не вечный Git-репозиторий.
- **Fork-able** — второй агент может разветвиться от текущего state без блокировки первого.
- **Merge-able** — когда параллельные fork'и сходятся, конфликты разрешаются LLM *как часть MCP-поверхности*, а не отдельным CLI-tool'ом.
- **Agent-native** — декларируемая цель работы, heartbeat-жизненный цикл, rich-hints «кто что трогает» в ошибках.
- **File-level, не record-level** — смешанный контент (код + бинарники + скриншоты + CSV + dumps моделей) — first-class.
- **Backed by standard storage** — каждый snapshot — это OCI-артефакт, который пушится в Zot/Harbor/GHCR и подписывается cosign. Без собственного реестра.
- **Offline-capable** — локальный реестр (Zot на localhost или directory-backed OCI image layout) полностью покрывает solo-developer сценарий без сети; remote sync включается только когда команда расширяется за пределы одной машины.

Ничего из OSS-ландшафта (середина 2026) не покрывает это всё вместе. Ближайшие соседи задокументированы в [RESEARCH-006](docs/architecture/research/RESEARCH-006-oss-prior-art.md).

## Чем это отличается

| Фича | Обычный agent FS (ctxfs, AgentFS, Fast.io) | Git-for-data (lakeFS, Pachyderm, Dolt) | **LOFS** |
|---|---|---|---|
| Эфемерный TTL | ⚠️ | ❌ | ✅ |
| Fork / merge | ❌ | ✅ (тяжёлый) | ✅ (lightweight, agent-scale) |
| Agent-native (MCP + intent) | ⚠️ | ❌ | ✅ |
| Cooperative multi-writer | ❌ | ❌ | ✅ (path-scoped intents через OCI Referrers) |
| Zero-infra (OCI-only, без БД) | ❌ | ❌ (нужен Postgres) | ✅ (БД — опциональное расширение) |
| OCI-artifact wire format | ❌ | ❌ | ✅ (interop с Zot/Harbor, cosign) |

## Три surface'а

LOFS поставляется как **один daemon с тремя surface'ами**, чтобы каждый потребитель использовал то, что ему удобнее:

| Surface | Аудитория | Транспорт | Когда использовать |
|---------|-----------|-----------|---------------------|
| **MCP server** (`lofs-mcp`) | LLM-агенты (Claude Code, Codex, Kimi, кастомные) | JSON-RPC over stdio / WebSocket | First-class UX для агента — tool calls внутри LLM-harness'а |
| **CLI** (`lofs`) | Люди и скрипты | Shell | DevOps, debugging, CI-задачи, ручная инспекция |
| **Agent Skills** (`skills/`) | Agent-harness'ы с skill-системой | Файлы-спеки | Drop-in best-practice паттерны (handoff, fan-out, checkpoint) |

### 1. MCP server — 4 L0 тула

На старте вся MCP-поверхность — это четыре tool'а. Всё остальное — опциональные эволюции, добавляемые только когда метрики покажут необходимость.

```
lofs.create({ name, ttl_days, size_limit_mb }) -> bucket_id

lofs.list({ org?, filter? }) -> BucketInfo[]     # включает текущий lock, активные fork'и, размер, hints

lofs.mount({
  bucket_id,
  mode: "ro" | "rw" | "fork",
  purpose: string,              # free-text — зачем агенту нужно
  scope?: string[],             # path-глобы, в которые агент собирается писать (для rw)
  expected_duration_sec: u32,
  ack_concurrent?: bool,        # на retry — подтверждаем что видим соседей
}) -> { mount_path, session_id } | MountAdvisory { neighbours[], hints[] }

lofs.unmount({
  session_id,
  action: "commit" | "discard",
  message?,
  conflict_policy?: "reject" | "scope_merge" | "fork_on_conflict",
}) -> { new_snapshot_id, neighbour_snapshots[] } | PushConflict { ... }
```

LOFS использует **cooperative-модель**, а не pessimistic lock. Каждый `mount --mode=rw` публикует **intent-манифест**, прикреплённый к тегу `:latest` бакета через OCI Referrers API. Следующий `rw`-mount пуллит актуальное состояние, видит активные intent'ы и либо (а) продолжает, если scope'ы не пересекаются, (б) возвращает `MountAdvisory` со списком соседей и советами (ждать / сузить scope / fork / повторить вызов с `ack_concurrent=true`). Commit — это операция в стиле `git pull --rebase`: пулл latest, diff против base (зафиксированного при mount), push нового слоя — соседи с дизъюнктными scope'ами сосуществуют, реальные пересечения возвращаются как `PushConflict`, который агент (или LLM в L1+) разрешает.

Никаких silent-failures, никаких скрытых lock'ов — и никакой БД на critical path. Полная модель — [ADR-002: Cooperative Coordination](docs/architecture/adr/ADR-002-cooperative-coordination.md).

### 2. CLI — `lofs`

Отражает MCP-поверхность плюс команды ops/dev. Удобно для скриптов, CI и отладки.

```bash
# Жизненный цикл bucket'а
lofs create <name> --ttl 7 --size-limit 1024    # ttl в днях, size в MB
lofs list [--org X] [--filter "..."] [--format json]
lofs stat <bucket>
lofs rm <bucket>

# Mount-сессия
lofs mount <bucket> --mode rw --purpose "implement OAuth2" --duration 600
lofs status                                      # мои активные сессии
lofs unmount <session_id> --commit "message"
lofs unmount <session_id> --discard

# Registry / подписи
lofs registry login <registry_url>
lofs registry push <snapshot> <ref>
lofs registry pull <ref>

# Dev / ops
lofs doctor                                      # health-check: userns, fuse3, registry auth
lofs gc                                          # expire bucket'ов, удаление orphan-blob'ов
lofs daemon                                      # запуск MCP-сервера (alias для `lofs-mcp`)
```

### 3. Agent Skills — готовые паттерны

Готовые [Agent Skills](https://agentskills.io/specification) для типовых multi-agent workflow'ов. Работают с Claude Code, Kimi CLI, OpenAI Codex и любым harness'ом, поддерживающим Agent Skills Standard. Установка через `lofs skills install` или просто копированием из `skills/` в `.claude/skills/` вашего проекта.

| Skill | Что делает |
|-------|------------|
| **lofs-handoff** | Передача работы между двумя агентами через выделенный bucket (A комитит → B читает) |
| **lofs-fan-out** | Оркестрация N sub-агентов с per-task bucket'ами + сбор результатов |
| **lofs-checkpoint** | Паттерн долгой задачи: периодические commit'ы, чтобы упавший агент мог быть восстановлен |
| **lofs-collective** | Три агента работают параллельно через fork'и + последовательные merge'и в основной bucket |
| **lofs-mount-discipline** | Best-practice гайд: короткие rw-сессии, batched local edits, явные purpose + duration |
| **lofs-review-merge** | (L1+) Паттерн agent-review для LLM-driven merge-предложений |

Каждый skill — это self-contained markdown-спека (`skill.md` + примеры), которая учит LLM когда и как использовать LOFS-примитивы в этом workflow.

Общий дизайн — [ADR-001-lofs.md](docs/architecture/adr/ADR-001-lofs.md). OCI-only модель координации — [ADR-002-cooperative-coordination.md](docs/architecture/adr/ADR-002-cooperative-coordination.md). Roadmap — [IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md).

## Roadmap эволюции (L1+)

Добавляется **только когда метрики покажут необходимость**:

| Уровень | Триггер | Добавляет |
|---|---|---|
| L1 | fork+merge workflow в реальном использовании | LLM-driven merge ladder (Identical → Mergiraf → LLM → Human) |
| L2 | storage cost > порога | Per-file BLAKE3 dedup + reference counting |
| L3 | dedup ratio < 2× | Content-defined chunking (FastCDC) + pack-файлы (zstd:chunked) |
| L4 | чтение частей больших файлов | Lazy-mount через SOCI-style zTOC + Range-GET |
| L5 | долгоживущие архивы | Tiered hot/cold storage (self-hosted hot + S3 Glacier) |
| L6 | агент путается в длинной истории | AI-generated cold-tier summaries + Ed25519 подписи |
| L7 | первый инцидент от auto-merge | HITL approval policy по sensitive paths |

## Под капотом

```
┌─────────────────────────────────────────────────────────────────────────┐
│   АГЕНТ (Claude Code / Codex / Kimi / …)     │  ЧЕЛОВЕК / CI-скрипты    │
│                                              │                           │
│   MCP-тулы (lofs.create/list/mount/unmount)  │  CLI `lofs`              │
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
              OCI-compatible реестр (Zot / Harbor / GHCR / GitLab)
              · :latest           → head snapshot manifest
              · :intent-<sid>     → эфемерные intent-манифесты (subject → :latest)
              · :snap-<ts>        → исторические snapshot'ы

     ┌──────────────────────────────────────────────────────────────┐
     │   агент внутри сессии:                                        │
     │   /mnt/lofs/<session>/    ← обычный POSIX-путь                │
     │   cat, grep, cargo, git, python — всё работает нативно        │
     └──────────────────────────────────────────────────────────────┘
```

**Ключевые библиотеки** (подробно в [RESEARCH-005](docs/architecture/research/RESEARCH-005-rust-oci-ecosystem.md)):

- [`oci-spec`](https://github.com/youki-dev/oci-spec-rs) + [`oci-client`](https://github.com/oras-project/rust-oci-client) — OCI-манифесты + регистри
- [`libfuse-fs`](https://crates.io/crates/libfuse-fs) + [`fuser`](https://github.com/cberner/fuser) — rootless userspace overlay
- [`ocirender`](https://edera.dev/stories/rendering-oci-images-the-right-way-introducing-ocirender) — streaming OCI layer merge с whiteouts
- [`fastcdc`](https://crates.io/crates/fastcdc) + [`zstd`](https://github.com/gyscos/zstd-rs) — chunking + compression (L3+)
- [`nix`](https://docs.rs/nix) + [`caps`](https://github.com/lucab/caps-rs) — user namespaces + Linux capabilities
- Sync `tar` crate — **никогда** tokio-tar (CVE-2025-62518)

## Quickstart (L0 dev-слайс)

L0 CLI-слайс (`create` / `list` / `stat` / `rm` поверх OCI registry)
работает уже сейчас — в комплекте docker-compose с двумя registry,
чтобы локально прогнать полный round-trip:

```bash
# 1. клонируем
git clone https://github.com/meteora-pro/lofs && cd lofs

# 2. поднимаем Zot + CNCF Distribution (localhost:5100 + :5101)
make dev-up

# 3. собираем CLI
cargo build -p lofs-cli --release

# 4. создаём bucket в Zot (дефолтный registry)
./target/release/lofs create demo --ttl-days 7

# 5. листинг — один и тот же бинарь против любого registry
./target/release/lofs list
./target/release/lofs --registry http://localhost:5101 list

# 6. интеграционные тесты против обоих registry
make test-e2e

# 7. сравнительные бенчмарки Zot vs Distribution
make bench

# 8. убираем всё
make dev-down
```

Конфигурация registry — [`docker/docker-compose.yml`](docker/docker-compose.yml).
Актуальная матрица поведения Zot / Distribution —
[`bench/registry-comparison.md`](bench/registry-comparison.md).

## Статус

Репозиторий — концепт-скелет. Дизайн на ревью. Не рекомендуется для реального использования. Реализация tracked через GitHub issues после принятия архитектуры.

Если интересна логика, приведшая сюда — читайте ADR и RESEARCH в [docs/architecture/](docs/architecture/).

## Документация

- [ADR-001: Дизайн LOFS](docs/architecture/adr/ADR-001-lofs.md) — общая архитектура + roadmap эволюции (L0 → L7)
- [ADR-002: Cooperative Coordination](docs/architecture/adr/ADR-002-cooperative-coordination.md) — OCI-only модель intent-манифестов, без обязательной БД
- [IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) — phased delivery plan + матрица библиотек + testing plan
- [Research-директория](docs/architecture/research/) — шесть deep-dive'ов по CRDT-FS, layered storage, координации, Rust-экосистеме, prior-art (исторические — для координации актуален ADR-002)
- [Contributing](CONTRIBUTING.md)

## Лицензия

Apache 2.0 — см. [LICENSE](LICENSE).

## Часть Meteora

- **[LOKB](https://github.com/meteora-pro/lokb)** — Local Offline Knowledge Base (permanent memory tier)
- **[devboy-tools](https://github.com/meteora-pro/devboy-tools)** — DevBoy MCP server с плагинной системой
- **lofs** (этот репо) — ephemeral shared workspace tier
