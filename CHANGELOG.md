# Changelog

All notable changes to LOFS are documented here. Format based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project
follows [Semantic Versioning](https://semver.org/) once past `0.1.0`.

## [0.0.2] — 2026-04-23

Sprint 2 — `RegistryDriver` per-flavour behaviour, GitLab support end-to-end,
GHCR/Harbor scaffolds, HTTP rate-limit policy.

### Added

- **`RegistryDriver` trait** (`crates/lofs-core/src/oci/driver/`) —
  per-flavour behaviour policy. Default methods encode standard OCI 1.1;
  specific drivers override only the bits that actually differ. Carries
  capability flags (`supports_artifact_type`, `supports_native_delete`,
  `catalog_supported`), `RateLimitPolicy`, and `effective_repo_mode`.
- **`GenericDriver`** — baseline OCI 1.1 (Zot, CNCF Distribution,
  Harbor with delete enabled). Auto-selected when no known hostname match.
- **`GitLabDriver`** — forces `RepoMode::Shared`, disables
  `artifactType` (GitLab CR media-type allow-list), and delegates DELETE
  through GitLab's REST API (`/api/v4/projects/:id/registry/repositories/:rid/tags/:tag`).
  Caches `(project_id, repo_id)` after the first resolve per session.
- **`GhcrDriver`** (scaffold) — auto-selected for `ghcr.io`; carries a
  tighter rate-limit policy (8 concurrent, 5s default backoff, 4 retries)
  to respect GHCR's ~5k authenticated req/h cap.
- **`HarborDriver`** (scaffold) — opt-in via `--driver harbor`.
  Inherits Generic behaviour today; module docs list the deviations
  that'll land as real Harbor deployments exercise the driver.
- **`HttpLimiter`** + `retry_on_429` (`oci/rate_limit.rs`) — optional
  semaphore-based concurrency cap plus automatic `Retry-After` handling
  (decimal seconds or RFC-2822 HTTP date). Every outbound registry call
  now flows through the driver's policy.
- **`lofs doctor`** output now prints the active driver, effective repo
  mode, and capability flags — makes it obvious *why* the client
  behaves a given way against a given registry.
- **CLI `--driver` flag** (`LOFS_DRIVER` env) with values
  `auto | generic | gitlab | ghcr | harbor`. Default `auto` picks by
  hostname.
- **Path-prefix support** in `--registry` URLs — `https://host/a/b`
  auto-selects `RepoMode::Shared` (bucket identity folds into a
  `<org>.<name>` tag on the `a/b` repo), matching what project-scoped
  registries require.
- **HTTP Basic auth** for the OCI registry client (`--username` +
  `--token`, env `LOFS_REGISTRY_USERNAME`) with transparent Bearer
  token-exchange for registries that challenge with
  `WWW-Authenticate: Bearer`.
- **GitLab API helper** (`oci/driver/gitlab/api.rs`) — thin
  `reqwest`-based client, PAT via `PRIVATE-TOKEN`, OAuth via `Bearer`,
  with shared-limiter-aware retries.
- **Opt-in live GitLab e2e** (`tests/it_gitlab.rs`) —
  `create/list/stat/rm` roundtrip against `registry.gitlab.com` when
  `LOFS_GITLAB_{URL,USERNAME,TOKEN}` env vars are set.
- **Docs:** registry compatibility matrix in
  [`bench/registry-comparison.md`](bench/registry-comparison.md) and the
  README, covering Zot / CNCF Distribution / GitLab / GHCR / Harbor
  across every capability.

### Changed

- **Custom `config.mediaType` dropped** from the pushed manifest. We
  now always publish the config blob under the standard
  `application/vnd.oci.image.config.v1+json` because GitLab CR (and
  some others) enforce a closed allow-list. Bucket identity still
  round-trips through manifest annotations (`pro.meteora.lofs.*`) —
  registries lose nothing, some (GitLab) gain compatibility.
- **OCI 1.1 `artifactType`** is no longer set on the manifest by
  default — same reason as above. Drivers that accept vendor
  `artifactType` can opt in via `supports_artifact_type()` in a later
  release.
- **`OciRegistry::anonymous_with_driver(url, driver)`** for callers
  that want to force a specific driver (`--driver` on CLI).

### Fixed

- **OCI `list_tags` on empty repositories** — GitLab returns
  `{ "tags": null }` which `oci-client` reports as a parse error. We
  now treat that as "no tags" so `lofs list` on a fresh project doesn't
  crash.
- **Manifest-NotFound on Zot** — Zot surfaces `NAME_UNKNOWN` (the
  repository stub lingers after a DELETE) where other registries
  surface `MANIFEST_UNKNOWN`; both now map to `LofsError::NotFound`.

### Known limitations

- **`mount` / `unmount`** — still scaffolded (Phase 1.2).
- **`lofs rm` on managed GitLab.com** — requires a PAT carrying the
  `api` scope (classic) or the "API" granular permission (fine-grained)
  on top of `read_registry` + `write_registry`. Error now surfaces
  GitLab's exact scope-request string, making it obvious which token
  to regenerate.

[0.0.2]: https://github.com/meteora-pro/lofs/releases/tag/v0.0.2

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
