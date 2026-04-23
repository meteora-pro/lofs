# LOFS Agent Skills

Drop-in behavioural skills for multi-agent workflows that use LOFS. Follow the [Agent Skills Standard](https://agentskills.io/specification) so they work with Claude Code, Kimi CLI, OpenAI Codex, and any harness that supports the spec.

> **Status:** planning stage. Skill scaffolds land together with Phase 1 (L0 MCP tools). This directory currently contains only the README and a single skeleton file — implementations come after the MCP surface stabilises.

## Planned skills

| Directory | Skill | Summary |
|-----------|-------|---------|
| `lofs-handoff/` | **lofs-handoff** | Transfer work from agent A to agent B through a dedicated bucket. A creates → commits → passes bucket_id → B mounts ro → reads. |
| `lofs-fan-out/` | **lofs-fan-out** | Orchestrator creates N per-subtask buckets, spawns sub-agents, awaits completion, collects artefacts. |
| `lofs-checkpoint/` | **lofs-checkpoint** | Long-running task pattern: periodic short rw-commits so a crashed agent is resumable from the last snapshot. |
| `lofs-collective/` | **lofs-collective** | Multi-agent parallel work: **scope-scoped `rw` mounts** (disjoint paths coexist) plus `mode=fork` for branching workflows. |
| `lofs-mount-discipline/` | **lofs-mount-discipline** | Best practice for agents: declare minimum `scope`, keep rw sessions short, batch local edits, set realistic `expected_duration`, react to `MountAdvisory` rather than retrying blindly. |
| `lofs-review-merge/` | **lofs-review-merge** | (L1+) Review a merge plan produced by the LLM-driven merge engine and apply overrides. |

## Layout of a single skill

```
lofs-handoff/
├── skill.md          # when to activate + instructions to the LLM
├── README.md         # human-readable description (optional)
└── examples/         # example transcripts / diffs (optional)
```

`skill.md` front-matter conventions follow the [Agent Skills Standard](https://agentskills.io/specification).

## Installation

Once the CLI lands in Phase 1:

```bash
# install all skills into ~/.claude/skills/ (or the harness-specific location)
lofs skills install

# install a single skill
lofs skills install lofs-handoff

# list available skills
lofs skills list
```

Manual install: just copy any skill directory into your harness's skills path (e.g. `~/.claude/skills/` or `.claude/skills/` at the project root).

## Contributing a skill

Skills are just markdown + optional examples. If you have a coordination pattern that works well on LOFS, open a PR with a new directory under `skills/` following the layout above.
