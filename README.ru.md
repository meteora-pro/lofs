# lofs — Layered Overlay File System для AI-агентов

[English](README.md) | [Русский](README.ru.md)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-concept-orange.svg)](#статус)

**LOFS** — легковесный rootless-бэкенд для **shared workspace** агентов AI, поверх OCI-реестров. Даёт агентам обычную POSIX-файловую систему, которую можно смонтировать, работать в ней и зафиксировать изменения как immutable-слой — с поддержкой fork/merge, ephemeral TTL и LLM-driven merge для конкурентных правок.

Можно думать о нём так: **`git worktree` + `docker image layer` + `object storage` — но специально под handoff, fan-out/fan-in и checkpoint-паттерны, которые нужны multi-agent системам.** Агент видит плоскую файловую систему (`/mnt/lofs/<session>`), а внутри каждый коммит — это OCI-слой, который пушится в любой Docker-compatible реестр (Zot, Harbor, GHCR).

> **Статус: концепция / скелет инфраструктуры.** Пока неиспользуемо. Дизайн-документы — в [`docs/architecture/adr/`](docs/architecture/adr/). Первая рабочая сборка: L0 (`lofs.create / list / mount / unmount`) — цель 2026-Q2.

## Семейство: LOKB ↔ LOFS

LOFS парный продукт к **[LOKB](https://github.com/meteora-pro/lokb)** — Local Offline Knowledge Base. Они закрывают два комплементарных слоя состояния агента:

| | **LOKB** | **LOFS** |
|---|---|---|
| **Что хранит** | Постоянные знания (заметки, факты, прошлые работы) | Эфемерный рабочий контекст (задачи, артефакты) |
| **Время жизни** | Годы | Часы – дни – недели |
| **Сеть** | Offline-first | Online by design (OCI registry, sync) |
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

Ничего из OSS-ландшафта (середина 2026) не покрывает это всё вместе. Ближайшие соседи задокументированы в [RESEARCH-006](docs/architecture/research/RESEARCH-006-oss-prior-art.md).

## Чем это отличается

| Фича | Обычный agent FS (ctxfs, AgentFS, Fast.io) | Git-for-data (lakeFS, Pachyderm, Dolt) | **LOFS** |
|---|---|---|---|
| Эфемерный TTL | ⚠️ | ❌ | ✅ |
| Fork / merge | ❌ | ✅ (тяжёлый) | ✅ (lightweight, agent-scale) |
| Agent-native (MCP + intent) | ⚠️ | ❌ | ✅ |
| LLM-driven merge как first-class | ❌ | ❌ | ✅ |
| OCI-artifact wire format | ❌ | ❌ | ✅ (interop с Zot/Harbor, cosign) |

## L0 API — 4 MCP тула

На старте вся surface, которую видит агент — это четыре tool'а. Всё остальное — опциональные эволюции, которые мы добавляем только когда метрики покажут необходимость.

```
lofs.create({ name, ttl_days, size_limit_mb }) -> bucket_id

lofs.list({ org?, filter? }) -> BucketInfo[]     # включает текущий lock, активные fork'и, размер, hints

lofs.mount({
  bucket_id,
  mode: "ro" | "rw" | "fork",
  purpose: string,              # free-text — зачем агенту нужно
  expected_duration_sec: u32,
  labels?: {...},
}) -> { mount_path, session_id } | MountError { holder, overlap_analysis, hints[] }

lofs.unmount({
  session_id,
  action: "commit" | "discard" | "merge",
  message?,
  resolutions?: [...],          # только на втором вызове если предыдущий unmount вернул конфликты
}) -> { new_snapshot_id } | MergeConflicts { ... }
```

`rw`-режим берёт эксклюзивный lock. Если он уже занят, вызывающий получает **rich-ошибку с purpose текущего holder'а, ожидаемым временем освобождения, LLM-сгенерированным overlap analysis и списком hints** (ждать / fork / попробовать ro / сузить scope). Никаких silent-failures, никаких скрытых lock'ов — у агента есть вся информация, которую хотел бы видеть человек-ревьюер.

Полный дизайн — [ADR-001-lofs.md](docs/architecture/adr/ADR-001-lofs.md).

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
┌────────────────────────────────────────────────────────────────────┐
│                        АГЕНТ (Claude Code, Codex, …)                │
│                                                                     │
│     lofs.mount ──> /mnt/lofs/<session>  (обычный POSIX-путь)        │
│                    агент: cat, grep, cargo, git, python …           │
└───────────────────────────────┬────────────────────────────────────┘
                                │ MCP
┌───────────────────────────────▼────────────────────────────────────┐
│                          lofs-daemon (Rust)                         │
│                                                                     │
│  libfuse-fs + ocirender     Postgres lock    oci-client / Zot        │
│  (rootless overlay mount)   (metadata)       (artifact push/pull)   │
└───────────────────────────────┬────────────────────────────────────┘
                                │
                                ▼
         OCI-compatible реестр (Zot / Harbor / GHCR / GitLab)
```

**Ключевые библиотеки** (подробно в [RESEARCH-005](docs/architecture/research/RESEARCH-005-rust-oci-ecosystem.md)):

- [`oci-spec`](https://github.com/youki-dev/oci-spec-rs) + [`oci-client`](https://github.com/oras-project/rust-oci-client) — OCI-манифесты + регистри
- [`libfuse-fs`](https://crates.io/crates/libfuse-fs) + [`fuser`](https://github.com/cberner/fuser) — rootless userspace overlay
- [`ocirender`](https://edera.dev/stories/rendering-oci-images-the-right-way-introducing-ocirender) — streaming OCI layer merge с whiteouts
- [`fastcdc`](https://crates.io/crates/fastcdc) + [`zstd`](https://github.com/gyscos/zstd-rs) — chunking + compression (L3+)
- [`nix`](https://docs.rs/nix) + [`caps`](https://github.com/lucab/caps-rs) — user namespaces + Linux capabilities
- Sync `tar` crate — **никогда** tokio-tar (CVE-2025-62518)

## Статус

Репозиторий — концепт-скелет. Дизайн на ревью. Не рекомендуется для реального использования. Реализация tracked через GitHub issues после принятия архитектуры.

Если интересна логика, приведшая сюда — читайте ADR и RESEARCH в [docs/architecture/](docs/architecture/).

## Документация

- [ADR-001: Дизайн LOFS](docs/architecture/adr/ADR-001-lofs.md) — полная архитектура L0/L1
- [Research-директория](docs/architecture/research/) — шесть deep-dive'ов по CRDT-FS, layered storage, координации, Rust-экосистеме, prior-art
- [Contributing](CONTRIBUTING.md)

## Лицензия

MIT — см. [LICENSE](LICENSE).

## Часть Meteora

- **[LOKB](https://github.com/meteora-pro/lokb)** — Local Offline Knowledge Base (permanent memory tier)
- **[devboy-tools](https://github.com/meteora-pro/devboy-tools)** — DevBoy MCP server с плагинной системой
- **lofs** (этот репо) — ephemeral shared workspace tier
