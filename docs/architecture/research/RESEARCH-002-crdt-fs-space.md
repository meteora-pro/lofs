---
id: RESEARCH-002
title: CRDT для распределённых файловых систем — пространство решений
date: 2026-04-22
tags: ["research", "crdt", "distributed-systems", "storage", "agents"]
related_adrs: ["ADR-001"]
---

# RESEARCH-002: CRDT для распределённых файловых систем

> Deep-dive для [ADR-001: Bucket-Teleport CRDT](../adr/ADR-001-lofs-crdt.md).
> Контекст: S3-backed workspace, multi-agent AI, периодический sync через CRDT, atomic transactional-shell + audit log.

---

## 1. CRDT для tree/filesystem structure

### Kleppmann 2021 — "A highly-available move operation for replicated trees"

- Алгоритм move-operation для tree CRDT без циклов, формально верифицирован в Isabelle/HOL.
- Статус: **production через обёртки**. Handwritten Scala из paper — ~1–2 µs local, Isabelle-generated ~50 µs. Remote ops O(n) avg, O(n²) worst-case.
- Реализации: **Loro** (`loro-dev/movable-tree` — основная прод-реализация 2025), **CodeSandbox/crdt-tree** (Rust, dormant), **trvedata/move-op** (reference).
- **Corner case**: undo механизм пересобирает undone-ops в tree → O(history × concurrent_moves) в глубокой истории. Для агентов, активно рефакторящих файлы — **performance cliff** на больших workspace.
- Followup: Kleppmann 2024 — "Extending JSON CRDTs with Move Operations" (move для вложенных JSON), планируется в Automerge.
- Ссылки: [Paper PDF](https://martin.kleppmann.com/papers/move-op.pdf) · [Loro movable-tree](https://github.com/loro-dev/movable-tree) · [Movable tree CRDTs blog](https://loro.dev/blog/movable-tree) · [JSON CRDT move 2024](https://martin.kleppmann.com/2024/04/22/json-crdt-move.html)

**Takeaway:** Kleppmann 2021 — core algorithm. Loro — быстрейшая реализация. Ограничить concurrent-move batch size, serialize moves через per-directory lock на HLC layer.

### Loro LoroTree

- Movable tree CRDT на базе Kleppmann 2021 + оптимизация через Event Graph Walker.
- Статус 2026: **1.0 released**, но Velt review 2026: *"Loro delivers strong performance but requires substantial development work and isn't production-ready"*. Активно развивается. Прод-юзеров масштаба Notion пока нет.
- Benchmark: 10K узлов, depth ≤ 4, 1M moves — log-spaced snapshots. Публичных цифр vs Automerge tree нет.
- Ссылки: [Loro docs Tree](https://www.loro.dev/docs/tutorial/tree) · [Loro 1.0 blog](https://loro.dev/blog/v1.0) · [Velt CRDT libs 2025](https://velt.dev/blog/best-crdt-libraries-real-time-data-sync)

**Takeaway:** Loro + LoroTree = лучший выбор для text+tree combo в Rust. Но — ждать breaking changes, **держать abstraction layer** для swap.

### Automerge tree

- Automerge 3.0 (август 2025): **10x memory reduction** (700 MB → 1.3 MB для Moby Dick), loading с 17h до 9s на больших историях.
- **Move-tree всё ещё experimental** в 3.x. API не stable.
- Ссылки: [Automerge 3.0](https://automerge.org/blog/automerge-3/) · [Automerge 3 memory](https://biggo.com/news/202508071934_Automerge_3.0_Memory_Improvements)

**Takeaway:** Automerge выигрывает в стабильности text CRDT, **но move-tree — не production**. Если нужна движуха узлов — Loro. Automerge как text-fallback.

### Alternative tree CRDTs

- **Yjs/Yrs**: нет native tree, эмулируется через nested maps, производит циклы при concurrent moves. **Избегать для FS**.
- **CodeSandbox `crdt-tree`**: dormant, только для learning.
- **Semantic / syntactic-validity-preserving CRDTs**: пусто, open question. Cambria (schema evolution) + json-joy (JSON + validation) — ближайшее. AST-level valid merge никто не решил.
- Ссылки: [Yjs vs Loro](https://discuss.yjs.dev/t/yjs-vs-loro-new-crdt-lib/2567) · [json-joy](https://github.com/streamich/json-joy)

---

## 2. CRDT для text content

### Yrs vs Automerge vs Loro (2025–2026)

- **Yrs** (Rust port Yjs): самый зрелый — 1.36 M downloads, 75 versions. Wins low-latency realtime. Минус — хранит Version Vector + Delete Set per version = overhead.
- **Automerge 3.0**: 10x memory, columnar compressed store. Лучший "full history + audit". Slow cold load на новый клиент с большой историей.
- **Loro**: Eg-walker архитектура — не нужно держать CRDT в памяти для edit; block-based storage ~4 KB blocks. Быстрый import, низкая память. Richtext через `crdt-richtext` (Peritext + Fugue).
- **diamond-types** (josephg): "world's fastest text CRDT" — 5000x быстрее legacy. Plain text only. Основа braidfs.
- Ссылки: [Automerge 3.0](https://automerge.org/blog/automerge-3/) · [Yrs crates.io](https://crates.io/crates/yrs) · [Loro performance](https://loro.dev/docs/performance) · [diamond-types](https://github.com/josephg/diamond-types) · [Eg-walker paper](https://arxiv.org/abs/2409.14252) · [CRDT benchmarks](https://github.com/dmonad/crdt-benchmarks)

### Known issues на больших документах

- Yjs: linear memory growth (Delete Set + YDoc в RAM).
- Automerge ≤ 2.x: load часы (fixed 3.0).
- Tombstone growth в sequence CRDTs (см. §8).

**Takeaway:** файлы > 5 MB — шардировать, не держать один Automerge doc на гигабайт.

### Интероп между ними

**Пусто, open question.** Нет кросс-формата. Automerge ↔ Loro = потеря истории или конвертация через JSON snapshot.

---

## 3. CRDT + filesystem — существующие попытки

### ElmerFS (Scality + INRIA, 2021) — **прямая академическая параллель**

- FUSE на Rust поверх AntidoteDB (JSON CRDTs). POSIX-compliant. Активная-активная гео-реплика.
- Paper: "CRDTs for Truly Concurrent File Systems" (HotStorage 2021).
- **viewID pattern**: конфликт при дупликатных именах → `file.txt__<uuid>` (вместо потери одной из копий).
- Issue: cycles при rename не до конца решены.
- Ссылки: [Scality/elmerfs](https://github.com/scality/elmerfs) · [HotStorage paper](https://www.lip6.fr/Marc.Shapiro/papers/2021/CRDT-filesystem-HotStorage-2021.pdf) · [AntidoteDB/crdt-filesystem](https://github.com/AntidoteDB/crdt-filesystem)

**Takeaway:** академический прототип **той же архитектуры**. Читать дословно. **Заимствовать viewID pattern** для concurrent-name resolution.

### IPFS / OrbitDB / Berty / Threads DB

- OrbitDB: Merkle-CRDTs + IPFS+libp2p pubsub. **Alpha, security audit не было**. Sync issues с IPFS ≥1.17.0. 20 npm модулей → хрупко.
- Ссылки: [OrbitDB](https://github.com/orbitdb/orbitdb) · [berty/go-orbit-db](https://github.com/berty/go-orbit-db)

**Takeaway: избегать для AI-agent FS.**

### Earthstar + Willow Protocol (2025)

- Syncable data store, path-queries, TTL, fine-grained permissions, offline-first. Willow'25 — stable spec.
- Rust `willow-rs` переехал [codeberg worm-blossom](https://codeberg.org/worm-blossom/willow_rs) (октябрь 2025, org rename — не abandonment).
- Ссылки: [Willow](https://willowprotocol.org/) · [Earthstar](https://earthstar-project.org/) · [LWN Earthstar](https://lwn.net/Articles/1005639/)

**Takeaway:** watch, **не строить на нём сейчас** — prod-юзеров нет.

### automerge-repo

- **Не production-ready** (заявлено на site). Подходит для прототипов, не для multi-agent FS.
- Next-gen sync: **Beelay + Keyhive** (Ink & Switch) — E2E-encrypted, server не видит данные, CGKA через BeeKEM. Для GAIOS.
- Ссылки: [automerge-repo](https://github.com/automerge/automerge-repo) · [Keyhive](https://www.inkandswitch.com/keyhive/notebook/) · [Beelay](https://github.com/inkandswitch/keyhive)

**Takeaway:** не использовать automerge-repo сейчас. Watch Beelay/Keyhive — интеграция когда стабилизируется.

### Braid Protocol + braidfs

- `braidfs` — bi-directional sync Braid HTTP resources ↔ local FS; diamond-types внутри. Active до апреля 2025.
- HTTP-only sync, не S3 native.
- Ссылки: [braidfs](https://github.com/braid-org/braidfs) · [Braid HTTP draft](https://datatracker.ietf.org/doc/draft-toomim-httpbis-braid-http/)

**Takeaway:** reference implementation для FS-level sync quirks.

### Jujutsu (jj) — **прямая модельная параллель**

- Operation-based, каждая op = snapshot view. Lock-free concurrent ops. Conflicts как first-class objects.
- GC (`jj util gc`) чистит unreachable, но operation log не repack'ается auto.
- `agentic-jujutsu` — multi-agent coding через jj, 10–100x быстрее concurrent Git.
- Ссылки: [jj operation log](https://jj-vcs.github.io/jj/latest/operation-log/) · [agentic-jujutsu](https://lib.rs/crates/agentic-jujutsu) · [Avoid Losing Work with jj](https://www.panozzaj.com/blog/2025/11/22/avoid-losing-work-with-jujutsu-jj-for-ai-coding-agents/)

**Takeaway:** **украсть operation log model дословно** — each FS-transaction = op record; view = CRDT root hash + metadata.

### Pijul (patch theory)

- Патчи формально коммутируют, графовая структура (CRDT-like). Apply O(p·c·log(h)).
- Когда лучше: semantic patch commutativity. Для FS — оверкилл.
- Ссылки: [Pijul theory](https://pijul.org/manual/theory.html)

### Sapling / EdenFS (Meta) — **паттерн lazy materialization**

- Virtual FS — FUSE / NFSv3 / ProjFS по OS. Inode либо non-materialized (fetchable by ObjectID), либо materialized (modified, overlay storage).
- Не CRDT, но урок: **никогда не загружать всё дерево сразу**. ObjectID (20 байт) + on-demand fetch.
- Ссылки: [Sapling Inodes](https://github.com/facebook/sapling/blob/main/eden/fs/docs/Inodes.md) · [Sapling overview](https://sapling-scm.com/docs/scale/overview/)

**Takeaway:** **применить паттерн lazy materialization** в lofs — агент видит FS, реально из S3 подтягивается только нужное.

### Dolt (Git for data, SQL)

- Merkle tree для SQL-данных. 50M rows + 1 row = инкрементально 1 row. Cell-based conflicts.
- Для file-level FS — оверкилл.
- Ссылки: [Dolt architecture](https://docs.dolthub.com/architecture/architecture)

---

## 4. Transactional shell на snapshots

### NixOS builds

- PID/mount/network/IPC/UTS namespaces для builder. Видна только Nix store + /dev+/proc+tmp/build. `__noChroot` escape hatch.
- Ссылки: [NixOS sandbox](https://mynixos.com/nixpkgs/option/nix.settings.sandbox)

**Takeaway:** взять mental model для "snapshot → execute → merge". Каждый агент = namespace-sandbox на snapshot'е.

### Bazel / Buck2 Remote Execution

- Action fingerprint = `hash(command+args+env+Merkle(inputs))`. Action cache keyed by digest. Outputs → CAS.
- Buck2: no local sandboxing by default, depends on remote exec.
- Ссылки: [Buck2 RE](https://buck2.build/docs/users/remote_execution/) · [REv2 API wiki](https://deepwiki.com/bazelbuild/remote-apis/2-remote-execution-api-v2)

**Takeaway:** **merkle-tree-of-inputs** для fingerprint каждого shell-transaction → ключ в audit log + dedup.

### btrfs/ZFS subvolumes для multi-agent

- Обе — COW snapshots, instant. btrfs менее удобный (subvolumes нельзя перемещать). ZFS — rename/move free.
- **Прод-юзеров для multi-agent FS публично нет** — белое пятно.
- Ссылки: [Btrfs vs ZFS Klara](https://klarasystems.com/articles/zfs-vs-btrfs-architects-features-and-stability-2/)

**Takeaway:** если self-hosted → ZFS subvolume per agent + CRDT merge для semantic layer.

### OverlayFS / devcontainer / Codespaces

- Gitpod, GH Codespaces, CodeSandbox — все изолируют per-user. **Shared workspace между агентами с concurrent writes — публично отсутствует**.

**Takeaway:** белое пятно, которое мы и закрываем.

### CRIU

- Persist running process state. `criu-coordinator` — distributed coordinated snapshots (Chandy-Lamport).
- Learning curve + только Linux.
- Ссылки: [CRIU AI agents](https://eunomia.dev/blog/2025/05/11/checkpointrestore-systems-evolution-techniques-and-applications-in-ai-agents/) · [criu-coordinator](https://github.com/checkpoint-restore/criu-coordinator)

**Takeaway:** не на старте. Применить для "pause agent + resume on other node" через 6+ месяцев.

---

## 5. Audit log / event sourcing

### Jujutsu operation log

- Append-only; op = snapshot "view" (bookmarks + heads + WC commit). `jj op log` — просмотр, `jj op restore` — откат. Нет auto-GC для op log objects.
- Ссылки: [jj operation log](https://jj-vcs.github.io/jj/latest/operation-log/)

**Takeaway:** копировать модель — each FS-transaction = op record.

### Event Sourcing (Fowler / Vernon)

- Append-only stream + projections. Для FS workable со snapshot interval (каждые N ops — full state), иначе replay взрывается.

### Datomic / XTDB

- Bitemporal: system time + valid time. Для FS в прямом смысле оверкилл, но valid-time может пригодиться для "кто что видел когда".
- XTDB — production-ready, open-source. Datomic — proprietary.
- Ссылки: [XTDB](https://xtdb.com/) · [XTDB bitemporality](https://v1-docs.xtdb.com/concepts/bitemporality/)

### CloudTrail Lake pattern

- Immutable store + SQL queries (до 7 лет). Gzip archives в S3.

**Takeaway:** **именно та модель** для audit log — S3 immutable + query layer (ClickHouse / DuckDB / Iceberg).

---

## 6. HLC / clock

### Состояние 2025

- **HLC** — стандарт для distributed DB (CockroachDB, YugabyteDB, MongoDB). 64 бита, tolerate NTP drift, default max skew 500ms.
- **Vector clocks** — O(N) на op, не годится для > 100 узлов.
- **Interval Tree Clocks (ITC)** — лучше для dynamic membership (агенты приходят-уходят), variable size, implementation complexity выше.
- **Bloom Clocks** — probabilistic, overkill для нас.

### Clock skew в cloud

- AWS/GCP с PTP ≤ 1ms; обычный NTP — 5–50ms в регионе, 100–200ms между регионами.
- CockroachDB crashes node если HLC > 500ms выше local physical. **500ms — reasonable default**, вынести в config.

### Rust crates

- **uhlc** (atolab) — самая популярная, 0.8.x, 500ms default, used in Zenoh (prod). **Production choice.**
- **hlc-rs** (tbg) — minimal, мало activity.
- **hlc_gen** — lock-free ref.
- Ссылки: [uhlc](https://crates.io/crates/uhlc) · [hlc-rs](https://github.com/tbg/hlc-rs) · [ITC](https://gsd.di.uminho.pt/members/cbm/ps/itc2008.pdf) · [CockroachDB clock mgmt](https://www.cockroachlabs.com/blog/clock-management-cockroachdb/)

**Takeaway:** **uhlc** — production-ready. Не самописать.

---

## 7. Rust-экосистема

### OpenDAL 2025–2026

- Production: Databend, RisingWave, GreptimeDB, sccache, Vector, Loco.
- Но **не 1.0** — breaking changes. `fuse3_opendal` + `ofs` — functional, но not perfected. Roadmap 2025: file versioning, e2e checksums, URI init.
- Ссылки: [OpenDAL roadmap 2025](https://opendal.apache.org/blog/2025/03/01/2025-roadmap/) · [fuse3_opendal](https://crates.io/crates/fuse3_opendal)

**Takeaway:** использовать как storage abstraction, pin conservative version, adapter layer.

### Iroh

- BLAKE3-verified streaming, blobs до ТБ. "Working in production" (200k concurrent conns). **v1.0 TBD**, iroh-blobs not prod quality yet — use 0.35.
- `iroh-docs` — document sync поверх blobs.
- Ссылки: [iroh](https://github.com/n0-computer/iroh) · [iroh-blobs](https://lib.rs/crates/iroh-blobs)

**Takeaway:** opt-in p2p-sync accelerator. S3 primary, Iroh как addon позже.

### Embedded KV

- **redb 4.1**: 920ms individual writes (лидер), 1.5x boost vs older, pure Rust, LMDB-inspired, stable. **Best pick.**
- **sled**: beta, известны space bloat issues.
- **RocksDB**: высокий write throughput, C++ bindings, меньше safety.
- Ссылки: [redb](https://github.com/cberner/redb) · [redb 4.1 release](https://www.webpronews.com/rusts-redb-hits-4-1-ai-agents-squash-bugs-deliver-1-5x-write-speedups-in-embedded-kv-store/)

**Takeaway:** **redb** для HLC/ops index. RocksDB только если нужно > 50k ops/s.

### Loro vs Automerge vs Yrs production

- Loro 1.0 — cutting-edge, not fully prod-ready (Velt 2026).
- Automerge 3.0 — stable text, tree experimental.
- Yrs — самый production, weak tree story.

**Takeaway:** Loro для workspace tree + text; Automerge как text fallback; Yrs не подходит для tree.

---

## 8. Gotchas / failure modes

### Tombstone growth / unbounded metadata

- Sequence CRDT растут пропорционально удалениям. Yjs хуже всех.
- GC требует синхронизации (две фазы / Paxos). Yorkie — causal stability; Loro — log-spaced snapshots + block storage.
- Ссылки: [Yorkie GC](https://github.com/yorkie-team/yorkie/blob/main/design/garbage-collection.md) · [CRDT GC](https://github.com/ipfs/notes/issues/407)

**Takeaway:** **GC с quorum-ack from day 0**. Threshold-based tombstone compaction, minimum 7 дней retention для audit.

### Semantically broken state

- MaxAverage: commutative, но `update(2) & update(4)` теряет `update(2)`.
- CRDT counter ≠ bank balance (может уйти в negative).
- Linked lists → broken links, cycles. Derive как view из list-CRDT.
- Ссылки: [Matt Weidner CRDT survey](https://mattweidner.com/2023/09/26/crdt-survey-2.html) · [CRDT Dictionary Duncan 2025](https://www.iankduncan.com/engineering/2025-11-27-crdt-dictionary/)

**Takeaway:** **не CRDT-ить бизнес-инварианты**. CRDT — только для "структура + content". Инварианты (permissions, execution) — через HLC + compensation + explicit locks.

### Performance cliffs

- Automerge ≤ 2.x: часы на load 1M-op doc (fixed 3.0).
- Long-tail undo в move-tree: O(history × concurrent_moves).
- Yjs large Delete Set на doc с миллионами deletions.

### Adversarial / Byzantine

- Обычные CRDT **не Byzantine-tolerant**. Malicious агент может:
  - послать malformed ops → crash parser;
  - обойти causal order → inconsistent merge;
  - "воскресить" tombstone.
- Решения: **Making CRDTs BFT** (PaPoC 2022), **Blocklace** (2024) — DAG BFT CRDT с crypto hashes.
- Beelay/Keyhive — E2E + CGKA для access control.
- Ссылки: [Making CRDTs BFT](https://dl.acm.org/doi/abs/10.1145/3517209.3524042) · [Blocklace](https://arxiv.org/html/2402.08068) · [Keyhive](https://www.inkandswitch.com/keyhive/notebook/)

**Takeaway:** в lofs агенты подконтрольные (не full Byzantine), но:
- **sign каждый op** Ed25519 по agent key;
- schema-validate ops до apply;
- quarantine unknown-signature ops в attestation queue;
- watch Beelay/Keyhive для access control.

### Riak production lesson

- Riak 2012 — CRDT pioneer. Компания схлопнулась, проект заброшен.
- **Не зависеть от одной CRDT-библиотеки** — abstraction layer чтобы смена engine не ломала API.

---

## 9. Semantic / schema-aware merge

### Cambria (Ink & Switch)

- JS/TS lib + bidirectional lenses для JSON schema evolution. Integrated Automerge.
- *"Document has no canonical schema — just log of writes from many schemas."*
- Status: research, мало prod-юзеров.
- Ссылки: [Cambria](https://www.inkandswitch.com/cambria/) · [Cambria DL](https://dl.acm.org/doi/pdf/10.1145/3447865.3457963)

### Tree-sitter-based merge — **критичный инсайт**

- **Mergiraf** — tree-sitter AST matching + GumTree. Syntax-aware merge driver для Git.
- **Weave** (Ataraxy-Labs, 2025) — entity-level granularity (function/class). На 31 real-world merges Weave решает все, Mergiraf часть валится.
- jj discussion #8831 — добавить Weave как complement.
- Ссылки: [Mergiraf](https://mergiraf.org/architecture.html) · [Weave](https://github.com/Ataraxy-Labs/weave) · [LWN Mergiraf](https://lwn.net/Articles/1042355/)

**Takeaway:** **Mergiraf для structured-file merges** в lofs → current state-of-art для code-file conflicts.

### diff3 + CRDT combos

**Пусто, open question.** Самописный layer.

---

## 10. Конкурентные решения 2025–2026

### AWS S3 Files (re:Invent 2025, GA April 2026) — **прямой конкурент**

- S3 bucket mount "as local FS" для AI agents. Решает object-file split для agent pipelines.
- Native file ops поверх S3, без separate storage system.
- **Без CRDT-merge, без multi-agent concurrent writes coordination, без audit log как first-class.**
- Ссылки: [VentureBeat S3 Files](https://venturebeat.com/data/amazon-s3-files-gives-ai-agents-a-native-file-system-workspace-ending-the) · [InfoWorld AWS S3 for AI](https://www.infoworld.com/article/4155868/aws-turns-its-s3-storage-service-into-a-file-system-for-ai-agents.html)

**Takeaway:** наш differentiation = **CRDT + transactional shell + audit log + cross-provider**. AWS не закрывает concurrent multi-agent writes.

### Cloudflare Project Think

- Tier 0 = Workspace — durable virtual FS на SQLite + R2. Read/write/edit/search/grep/diff.
- Centralised CF-hosted.
- Ссылки: [CF Project Think](https://blog.cloudflare.com/project-think/)

### OpenAI Agents SDK sandbox (April 2026)

- Mount S3/GCS/Azure/R2. `memory.py` / `memory_s3.py` / `memory_multi_agent_multiturn.py` — memory layouts per agent.
- Ссылки: [OpenAI sandbox docs](https://developers.openai.com/api/docs/guides/agents/sandboxes) · [OpenAI agents SDK evolution](https://openai.com/index/the-next-evolution-of-the-agents-sdk/)

**Takeaway:** direct reference — читать их code.

### VAST AgentEngine (2026)

- Production multi-agent platform, full auditability. Closed, enterprise.

### CRDT-SQL за пределами Dolt

**Пусто, open question.**

---

## Bibliography: must-read топ-10

1. **Kleppmann, Mulligan 2021** — A highly-available move operation for replicated trees. [PDF](https://martin.kleppmann.com/papers/move-op.pdf)
2. **Gentle, Kleppmann 2024** — Collaborative Text Editing with Eg-walker. [arXiv](https://arxiv.org/abs/2409.14252)
3. **Kleppmann 2024** — Extending JSON CRDTs with Move Operations. [Blog](https://martin.kleppmann.com/2024/04/22/json-crdt-move.html)
4. **"CRDTs for Truly Concurrent File Systems"** (ElmerFS, HotStorage 2021). [PDF](https://www.lip6.fr/Marc.Shapiro/papers/2021/CRDT-filesystem-HotStorage-2021.pdf)
5. **Peritext** (Litt/Lim/Kleppmann/vanHardenberg 2022) — rich-text CRDT. [PDF](https://dspace.mit.edu/bitstream/handle/1721.1/147641/3555644.pdf)
6. **"Making CRDTs Byzantine fault tolerant"** (PaPoC 2022) + **Blocklace 2024**. [ACM](https://dl.acm.org/doi/abs/10.1145/3517209.3524042) · [arXiv](https://arxiv.org/html/2402.08068)
7. **Jujutsu operation log** + design. [docs](https://jj-vcs.github.io/jj/latest/operation-log/)
8. **Loro movable-tree** + Eg-walker docs. [repo](https://github.com/loro-dev/movable-tree) · [Eg-walker](https://loro.dev/docs/advanced/event_graph_walker)
9. **Matt Weidner CRDT Survey Part 2: Semantic Techniques**. [Blog](https://mattweidner.com/2023/09/26/crdt-survey-2.html)
10. **Sapling EdenFS "Inodes"** + overview. [GitHub](https://github.com/facebook/sapling/blob/main/eden/fs/docs/Inodes.md)

---

## Топ-5 рисков проекта

1. **Tombstone growth / unbounded metadata** — без quorum-acked GC storage и sync-size взорвутся за недели. *Митигация:* GC с day 0, log-spaced snapshots + threshold-based tombstone compaction, 7d audit retention.
2. **Move-tree cliffs при concurrent reorganizations** — O(history × concurrent_moves) undo cost. AI-агенты массово рефакторят. *Митигация:* ограничить batch size, serialize moves per-directory lock на HLC, кешировать concurrency window.
3. **Semantic breakage: converged-but-invalid state** — CRDT-merge двух edit'ов `package.json` → невалидный JSON. Binary — хуже. *Митигация:* **default — LWW-HLC + Mergiraf**; CRDT text — opt-in только для explicit collaborative файлов.
4. **Lock-in в experimental CRDT lib** — Loro/Automerge 3 не fully production. Риск Riak-scenario. *Митигация:* `MergeEngine` trait с pluggable backends; on-disk формат = собственный event log + CRDT как cache; snapshot в plain JSON/bincode для recovery.
5. **Adversarial agent / malformed op injection** — скомпрометированный agent crashes merge или резуррекит deleted files. *Митигация:* Ed25519-sign каждый op по agent key, schema-validate до apply, quarantine unknown-signature ops, watch Beelay/Keyhive для integration.

---

## Обновлённая рекомендация по стеку

### Change / Confirm

- **CRDT tree + text**: **Loro 1.x** primary. Fallback Automerge 3.0 (text only). diamond-types если text окажется bottleneck. **Не Yjs/Yrs для tree**.
- **HLC**: **uhlc**, 500ms drift default. **Не** самописать.
- **Embedded KV для HLC index + op log**: **redb 4.1**. Не sled, не RocksDB.
- **Storage abstraction**: **OpenDAL** + `fuse3_opendal` для FUSE. Pin version + adapter layer.
- **Content-addressable p2p**: Iroh 0.35 как **opt-in** accelerator. S3 primary.
- **Virtual FS / lazy materialization**: EdenFS pattern — FUSE поверх OpenDAL + local CAS cache.
- **Transaction sandbox**: NixOS-style namespaces per agent + ZFS subvolume (self-hosted) / OverlayFS+btrfs (cloud).
- **Audit log storage**: S3 immutable + **ClickHouse или DuckDB+Iceberg** для query (CloudTrail Lake pattern). Не чистый Postgres.
- **Semantic merge structured files**: **Mergiraf** plug-in merge driver; coexist с CRDT.
- **Operation log design**: **jj + ElmerFS** как дословный reference.

### Изменения относительно начального плана ADR-001

- **Postgres → только control plane** (users/sessions/agents/ACL). HLC/op log → **redb**.
- Добавить **viewID-suffix** для concurrent same-name files (ElmerFS).
- **Не использовать automerge-repo** как sync layer — свой sync over OpenDAL multipart + Iroh supplement.
- **Default content policy = LWW-HLC + Mergiraf**, CRDT text — opt-in. Это важное ужесточение.
- Добавить **lazy materialization (EdenFS pattern)** как core design principle — не загружать всё дерево сразу.
- Добавить **Ed25519 signing of ops** для Byzantine-resistance.

### Откладывать / watching

- Willow Protocol / Earthstar v11 — watch.
- Cambria schema evolution — когда workspace format стабилизируется.
- CRIU checkpoint — feature "pause agent", 6+ месяцев.
- Keyhive/Beelay — когда выйдет публично из GAIOS → integrate E2E + access control.
