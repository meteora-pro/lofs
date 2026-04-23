# Changelog

All notable changes to LOFS are documented here. Format based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project
follows [Semantic Versioning](https://semver.org/) once past `0.1.0`.

## [0.0.1] — 2026-04-23

First tagged release. L0 CLI slice for **ADR-001** / **ADR-002** — bucket
identity + metadata over any OCI-compatible registry, with no mandatory
database. Mount / unmount backends land in Phase 1.2.

### Added

- `lofs` CLI with `doctor`, `create`, `list`, `stat`, `rm` — all speak
  pure OCI Distribution API.
- `OciRegistry` core client — auto-detects repo-addressing mode from URL:
  `Separate` (dedicated LOFS registry, repo per bucket) or `Shared`
  (project-scoped registries like GitLab CR, identity folded into tag).
- Anonymous / Bearer / **HTTP Basic** auth (with transparent JWT token
  exchange for registries that require it — GitLab, Harbor).
- Path-prefix support in base URL (`https://registry/user/project`) so
  a single LOFS installation works against bare-host Zot and
  project-scoped GitLab side by side.
- Dev environment: `docker/docker-compose.yml` spins up Zot v2.1.2 and
  CNCF Distribution 3.0.0 side by side on ports 5100 / 5101. `Makefile`
  wraps common tasks (`dev-up`, `test-e2e`, `bench`, `docker-test-cli`).
- Integration test matrix: 16 end-to-end scenarios run against Zot *and*
  Distribution on every CI pass.
- Criterion benchmark comparing the two registries end-to-end —
  `bench/registry-comparison.md` has the behavioural matrix + numbers.
- Multi-stage `Dockerfile.cli` — Linux image (114 MB, debian:bookworm-slim
  runtime) published to `ghcr.io/meteora-pro/lofs/cli` on release.
- GitHub Actions CI — `fmt`, `clippy`, 5-platform `test-unit` +
  `build` matrix (Linux x64/arm64, macOS x64/arm64, Windows x64), plus
  `test-e2e` and `docker-cli` jobs.
- Release workflow producing signed-sum binaries for all five platforms
  on every `v*` tag.

### Design documents

- [ADR-001 v4.1](docs/architecture/adr/ADR-001-lofs.md) — design pivot:
  OCI-only storage, explicit L0 vs L1-L7 evolution split.
- [ADR-002](docs/architecture/adr/ADR-002-cooperative-coordination.md) —
  cooperative coordination model (intent manifests via Referrers API,
  path-scoped writes, no mandatory SQL/Redis).
- [IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) — phased roadmap
  with library usage matrix and testing plan.

### Known limitations

- `mount` / `unmount` — scaffolded, return `unsupported platform` (Phase 1.2).
- `lofs rm` against managed GitLab.com — GitLab closes the OCI DELETE
  manifest endpoint; delete via the project UI for now. Native GitLab
  API fallback tracked for Phase 1 follow-up (see `RegistryDriver` trait
  design).
- No OS keyring integration yet.

[0.0.1]: https://github.com/meteora-pro/lofs/releases/tag/v0.0.1
