---
id: RESEARCH-006
title: OSS prior-art — fork/merge workspace для AI-agents
date: 2026-04-22
tags: ["research", "competitive-analysis", "agents", "workspace", "oss"]
related_adrs: ["ADR-001"]
---

# RESEARCH-006: OSS prior-art для agent workspace fork-merge

> Skeptical search — существует ли готовое OSS/commercial решение нашей задачи. Если нет — где подошли ближе всего.

---

## TL;DR

**Gap НЕ полностью confirmed**. Space взорвался за последние 12 месяцев (Feb 2026: «every major tool shipped multi-agent in the same two-week window»). Но **никто не покрывает все 4 наши differentiators сразу**:

1. Ephemeral TTL
2. Fork/merge
3. Agent-native MCP + intent coordination
4. **LLM-driven merge as first-class primitive**

**Самый defensible differentiator — #4.** Никто не делает `merge.propose/suggest/override/execute` как MCP surface owned by workspace. Harmony (Source.dev) делает LLM-merge как CLI. Copilot — в PR UX. А не на уровне workspace primitive.

**Closest competitors (ранжировано):**
1. **Cloudflare Project Think + Artifacts (April 2026)** — tree forking + Git-like storage. **НЕТ LLM merge.** CF-locked.
2. **ConTree (Nebius, 2026)** — microVM Git-like branching. Apache 2.0 MCP SDK. **НЕТ merge** (только branch + keep winner + discard).
3. **Daytona (Series A $24M, Feb 2026)** — fork tree + volumes. **НЕТ merge**.
4. **lakeFS** — Git-for-data. Ops-weight слишком велик для ephemeral agent buckets.

---

## Gap-closure matrix — четыре differentiators × 20 solutions

| Solution | Ephemeral TTL | Fork/Merge | Agent-native (MCP+intent) | LLM-driven merge |
|---|---|---|---|---|
| **lofs / LOFS (we)** | ✅ 30d + tiered | ✅ fork + 4-tier merge | ✅ MCP + 7-state intent | ✅ Tier 3 + 8 merge tools |
| **lakeFS** | ❌ long-lived | ✅ branch + merge (file-level) | ❌ data-eng API | ❌ three-way only |
| **Pachyderm** | ❌ persistent | ✅ branch + merge (pipeline) | ❌ K8s-native | ❌ |
| **Nessie** | ❌ persistent | ✅ catalog branch + merge | ❌ table-only | ❌ |
| **Dolt** | ❌ persistent | ✅ table branch + merge | ❌ SQL-only | ❌ |
| **Daytona** | ✅ ephemeral sandboxes | ⚠️ fork only, no merge | ⚠️ sandbox API | ❌ |
| **E2B** | ✅ 14d persistence | ❌ pause/resume only | ⚠️ SDK | ❌ |
| **ConTree (Nebius)** | ⚠️ checkpoint-based | ✅ branch + rollback, no merge | ⚠️ MCP + SDK | ❌ |
| **Freestyle VMs** | ✅ ephemeral | ⚠️ fork only | ⚠️ API | ❌ |
| **CodeSandbox SDK** | ⚠️ hibernate | ⚠️ fork only | ⚠️ SDK | ❌ |
| **Cloudflare Project Think** | ✅ Durable Objects | ✅ fork tree | ⚠️ Workers-bound | ❌ |
| **Cloudflare Artifacts** | ❌ persistent | ✅ Git fork; merge unclear | ⚠️ API | ❌ |
| **AWS S3 Files** | ⚠️ AWS-only | ❌ | ⚠️ NFS | ❌ |
| **Fast.io** | ❌ persistent | ❌ | ✅ MCP-native | ❌ |
| **Ona (ex-Gitpod)** | ✅ ephemeral env | ⚠️ git-based | ⚠️ agent platform | ❌ |
| **Imbue Sculptor** | ⚠️ container-scope | ⚠️ container isolation | ✅ agent-native | ❌ |
| **Claude Code worktrees** | ❌ local | ⚠️ git-native | ✅ agent-native | ⚠️ via LLM in IDE |
| **LangGraph Deep Agents** | ⚠️ thread-scope | ❌ | ✅ middleware | ❌ |
| **agentjj (jj)** | ❌ local VCS | ✅ jj branch + merge | ⚠️ via skills | ❌ |
| **Keyhive/Beelay** | ⚠️ local-first | ❌ CRDT sync | ❌ research | ❌ |
| **CodeCRDT (academic)** | ⚠️ session | ⚠️ observation-driven | ⚠️ prototype | ❌ |
| **Harmony / LLMinus / Copilot** | N/A (merge tool) | N/A | N/A | ✅ LLM merge (но не workspace) |

**Никто** не имеет все 4 green. 4-way combination — **where novelty lives**.

---

## Key новые findings (которые изменили картину)

### ConTree (Nebius, 2026) — новое обнаружение

[ContTree.dev](https://contree.dev/) · [docs](https://docs.contree.dev/)

**Direct conceptual match.** «Sandbox for AI agents with Git-like branching.» MicroVM isolation. Branch N times, run parallel, score, instant rollback. MCP server + Python SDK — **Apache 2.0 open source**. Commercial managed service.

**Gaps vs LOFS:**
- Нет merge (branch + keep winner + discard rest).
- Нет LLM-merge.
- Нет multi-agent coordination primitives.
- Нет ephemeral TTL semantics.
- Нет OCI artifacts.
- Нет content-addressable dedup между buckets.
- Snapshot = whole-VM filesystem image, не file-level content-addressable tree.

**Может быть использован как sandbox layer под нашим storage.**

### Cloudflare Project Think + Artifacts (April 2026)

[Project Think](https://blog.cloudflare.com/project-think/) · [Artifacts beta](https://blog.cloudflare.com/artifacts-git-for-agents-beta/)

**Самый dangerous competitor.** Tier 0 Workspace = durable virtual FS (SQLite+R2). Conversations stored as trees с parent_id → forking built-in. Non-destructive compaction. Artifacts — «Versioned file system that speaks Git». Programmatic repo creation, `repo.fork(...)`. Git clients + REST/TS APIs.

**Gaps vs LOFS:**
- CF-locked (Durable Objects, Workers).
- Нет LLM-driven merge.
- Merge в Artifacts не описан (только fork documented).
- Нет explicit multi-agent coordination (intent/heartbeat).
- Нет OCI artifact interop.
- Нет cross-cloud.

**Kill-risk #1**: если CF добавит LLM merge + coordination → Project Think становится full competitor.

### Harmony (Source.dev / Android) — доказательство LLM-merge в проде

[Harmony AI](https://www.source.dev/journal/harmony-preview)

88-90% merge-конфликтов auto-resolved. Agentic orchestrator + context retrieval + structured reasoning + validation tools. **Только для кода, one-shot CLI.** Не workspace primitive.

**Значит:** LLM-merge в проде работает, 88-90% success rate — реальный benchmark для нашей цели.

### Abandon Git LFS для AI Agents

[Justin Poehnelt blog](https://justin.poehnelt.com/posts/abandon-git-lfs-because-agents/)

Публичное признание что LFS **actively ломается** с agent sandboxes (proxy issues). Валидирует часть нашей мотивации (git+LFS — wrong primitive для agent workloads).

### Claude Code worktrees (2026) — industry converge point

`-w` flag + `isolation: worktree` frontmatter на subagents. Built-in orchestration. Ecosystem тулов: `agentree`, `ccswarm`, `gwq`.

**Значит**: индустрия converging на «worktree + coordinator + quality gates», НЕ на novel storage primitives. Это для LOFS — opportunity: мы заходим в другой quadrant (ephemeral blob workspace с LLM-merge), а не конкурируем с worktree-based flows.

### Cloudflare Artifacts "Git for Agents" — дословно наша формулировка

Публичная beta с апреля 2026. Git fork через API. Merge не документирован. **Мы должны watch очень внимательно.**

---

## «Why not X?» — checked ответы

### lakeFS (storage backend option)

- Operational weight: lakeFS хочет own Postgres + API + control plane. Ephemeral agent-scale (100+ buckets/day) — не его workload.
- Merge — plain three-way file-level, нет LLM-ladder.
- Нет MCP-native API.
- Нет intent/coordination primitives.
- Нет OCI export/import.
- **Может быть embedded storage behind OpenDAL** — но не MCP-facing primitive.

### Git + LFS + daemon

- LFS actively breaks в agent sandboxes (proxy).
- Нет TTL.
- Submodule боль (ADR-001 known).
- Нет semantic merge layer.
- Good for code, плохо для mixed-content.

### OCI image layers plain

- Linear parent chain, не DAG. Whiteout convention. Copy-up.
- Мы **borrow** media-type + subject-reference + artifact model.
- Нет merge, agent model, TTL.
- Registry (Zot/Harbor) — наш publish target, не workspace runtime.

### Daytona + Volumes

- Fork = full VM duplication (FS + memory). Heavy.
- Нет merge — только fork + discard winner.
- Volumes — flat FUSE → S3, нет versioning.
- Vendor-locked (OSS частично).

### ConTree (потенциальный конкурент или integration)

- Closest architectural match. MicroVM + branching + MCP + Apache 2.0 SDK.
- Snapshots = whole-VM images, не content-addressable trees.
- Нет merge, нет coordination primitives.
- **Integration option**: ConTree как sandbox layer, LOFS как storage/merge/coordination layer on top.

### Cloudflare Project Think

- CF-locked. Нет LLM merge. Нет multi-agent coordination.
- **Kill-risk #1**: если CF shipment LLM merge + intent coordination.

### Claude Code git worktrees

- Worktrees share `.git` object DB — ломается с 100GB блобами и binary.
- Нет cross-agent handoff primitive beyond «commit + push + pull».
- Нет ephemeral TTL.
- **LOFS positioning: complementary layer** (cross-worktree artifact bus + persistent mixed-content blob-pool + LLM-merge engine для handoff). Не замена git.

---

## Kill criteria (если кто-то shipment первым)

| # | Scenario | Probability | Mitigation |
|---|----------|-------------|-----------|
| 1 | **CF Project Think adds LLM merge + intent** | Moderate-high (CF двигается быстро) | Cross-cloud (OpenDAL → MinIO/S3/GCS/R2) + OCI interop + Zot/Harbor — не locked в CF |
| 2 | **Daytona adds merge semantics** | Low-moderate | 4-tier LLM merge + 8 tools non-trivial to replicate; Daytona sandbox-compute focus |
| 3 | **ConTree adds coordination + merge** | Moderate (Nebius team capable) | Open-source velocity + DevBoy ecosystem integration + MCP interop |
| 4 | **lakeFS pivots to agent-ephemeral** | Low ($20M enterprise AI push possible) | MCP-native + ephemeral + LLM merge + OCI — не в их DNA |
| 5 | **Anthropic ships managed agent workspace** | Low-moderate (Managed Agents April 2026) | Интегрируемся с Claude, не конкурируем |

---

## Strategy suggestion (integrate, don't isolate)

1. **Differentiators real but narrow.** Ship Phase 1-3 fast для demoable LLM-driven merge. Это main moat.
2. **Consider integrating lakeFS/Project Think как storage backend** behind OpenDAL — если matures, swap. Не изобретать blob-store если можно delegate.
3. **Closely watch ConTree, Cloudflare, Daytona.** Они наиболее likely добавить merge. Build OpenAPI/MCP-compat layer для interop.
4. **Position complementary к Claude Code worktrees** — LOFS = cross-worktree artifact bus + mixed-content blob-pool + LLM-merge для handoff. Not replacing git.
5. **Open-source рано.** Apache 2.0 на `lofs-core` + MCP tools. Community feedback тестирует novelty claim.
6. **Partnership, not fork**: Mergiraf/Weave community (consumers via subprocess). Contribute к agentjj design discussions.
7. **Benchmark против ConTree + Daytona + CF** на MAST-inspired test matrix (100 agents × 10 buckets × 14 failure modes). Если numbers dramatically лучше на coordination-breakdown категории (36.9% failures per MAST) — markeble.
8. **Если direct LLM-merge-as-MCP-primitive product launch в 3-6 мес** — consider fork/join rather than build from scratch.

---

## Verdict

### Genuinely novel quadrants (gap confirmed)

1. **LLM-driven merge as first-class MCP primitive** встроенный в workspace lifecycle, 4-tier + 8 tools + quarantine + review-centric HITL.
2. **Intent lifecycle** (7 states + heartbeat + max-hop + negotiation + drift verification) как MCP coordination primitive bound к workspace.
3. **OCI-artifact wire format** для sharing/publishing между orgs/clouds с cosign keyless.
4. **Tiered retention с AI-generated summaries в cold tier** + Ed25519-signed + archive-pointer recovery.

### Close proxies to borrow

- **lakeFS** — fork/merge pattern over object storage.
- **ConTree** — microVM branching UX.
- **CF Project Think** — tree-with-parent_id + durable filesystem UX.
- **Jujutsu / agentjj** — first-class conflicts.
- **Harmony / LLMinus / Mergiraf / Weave** — direct reuse через subprocess.
- **MemGPT / MemPalace** — recursive summary memory template.
- **Nydus/SOCI** — chunked CAS + Range-GET pattern.
- **Restic / Borg** — pack-file + chunking + prune-compact.

---

## Sources

Ключевые новые сources (полный список в task output):

- [Cloudflare Project Think](https://blog.cloudflare.com/project-think/)
- [Cloudflare Artifacts Git for Agents beta](https://blog.cloudflare.com/artifacts-git-for-agents-beta/)
- [ConTree.dev](https://contree.dev/) · [docs](https://docs.contree.dev/)
- [Daytona Series A Feb 2026](https://www.alleywatch.com/2026/02/daytona-ai-agent-infrastructure-sandbox-computing-developer-tools-ivan-burazin/)
- [Daytona Sandboxes docs](https://www.daytona.io/docs/en/sandboxes/)
- [Freestyle VMs](https://www.freestyle.sh/products/vms)
- [CodeSandbox SDK Fork](https://codesandbox.io/docs/sdk/fork)
- [Sculptor (Imbue)](https://github.com/imbue-ai/sculptor)
- [Ona (ex-Gitpod)](https://ona.com)
- [Cognition Devin 2.0](https://cognition.ai/blog/devin-2)
- [lakeFS](https://github.com/treeverse/lakeFS) · [AI agents article](https://lakefs.io/blog/ai-agents/)
- [Dolt Git remote support Feb 2026](https://www.dolthub.com/blog/2026-02-13-announcing-git-remote-support-in-dolt/)
- [agentjj](https://github.com/2389-research/agentjj)
- [Harmony AI (Source.dev)](https://www.source.dev/journal/harmony-preview)
- [Abandon Git LFS because AI Agents](https://justin.poehnelt.com/posts/abandon-git-lfs-because-agents/)
- [LangGraph Deep Agents backends](https://docs.langchain.com/oss/python/deepagents/backends)
- [Microsoft Copilot Cowork March 2026](https://www.microsoft.com/en-us/microsoft-365/blog/2026/03/09/copilot-cowork-a-new-way-of-getting-work-done/)
- [Anthropic Multi-Agent Research System](https://www.anthropic.com/engineering/multi-agent-research-system)
- [Anthropic Managed Agents April 2026](https://www.infoq.com/news/2026/04/anthropic-managed-agents/)
- [MAST NeurIPS 2025](https://neurips.cc/virtual/2025/loc/san-diego/poster/121528)
- [CodeCRDT arXiv 2510.18893](https://arxiv.org/abs/2510.18893)
- [OpenHands SDK paper](https://arxiv.org/html/2511.03690v1)
- [Claude Code parallel worktrees 2026](https://popularaitools.ai/blog/claude-code-git-worktrees-parallel-coding-2026)
- [ccswarm GitHub](https://github.com/nwiizo/ccswarm)
- [Plastic SCM Merge Machine](https://www.plasticscm.com/mergemachine)
- [GitHub Copilot merge conflicts 2026](https://github.blog/changelog/2026-04-13-fix-merge-conflicts-in-three-clicks-with-copilot-cloud-agent/)
