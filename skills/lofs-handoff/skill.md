---
name: lofs-handoff
description: Передать работу от одного агента другому через LOFS bucket. Agent A накапливает артефакты в bucket'е и коммитит; agent B импортирует snapshot и читает.
when_to_use: Когда LLM-агент завершает этап работы и следующий шаг должен выполнить другой агент (человек или автомат), которому нужен собранный контекст, а не чистый conversation handoff.
tools: lofs.create, lofs.mount, lofs.unmount, lofs.list
---

# lofs-handoff — skill scaffold

> **Status:** scaffold. Full content lands together with Phase 1 MCP tools.

## When to activate

Use this skill when:

- You are an agent finishing a task (research, draft, analysis) and a **different** agent (or a human reviewer) needs to pick it up.
- The artefacts include **files** (markdown, screenshots, CSVs, patches, code) — i.e. the handoff is heavier than a conversation summary.
- Both agents share access to the same LOFS daemon / OCI registry.

## Flow (planned)

```
Agent A:
  1. lofs.create(name="dev-666-research", ttl_days=7)              → bucket_id
  2. lofs.mount(bucket_id, mode="rw",
                purpose="research OAuth refactor options",
                scope=["/**"],
                expected_duration_sec=1800)                         → mount_path, session
  3. Work in mount_path using normal tools (cat, grep, cargo…)
  4. lofs.unmount(session, action="commit",
                   message="research complete")                     → snapshot_id
  5. Pass bucket_id (and optionally snapshot_id) to agent B.

Agent B:
  1. lofs.mount(bucket_id, mode="ro",
                purpose="implement MR per handoff")                 → mount_path, session
  2. Read the artefacts, produce implementation.
  3. lofs.unmount(session, action="discard").
```

## Guard rails

- Declare the real `purpose` and the **narrowest sensible `scope`** — neighbours will see both via `lofs.list` (the advisory hints are only as useful as the info you publish).
- Keep rw sessions **short** — discipline matters (see `lofs-mount-discipline`).
- If agent B needs to extend the work **concurrently** with A, it has three options:
  1. Wait until A's heartbeat expires or commit arrives.
  2. Use a non-overlapping `scope` — agents can mount `rw` in parallel as long as scopes are disjoint (see ADR-002).
  3. Use `mode="fork"` to branch off without contending for the `:latest` tag (see `lofs-collective`).
- On receiving `MountAdvisory`, parse `neighbours[]` and `hints[]` from the structured payload and pick a course of action — don't retry blindly.

## Example transcript

_To be added once the tools land._
