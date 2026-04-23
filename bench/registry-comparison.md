# LOFS registry benchmark — Zot vs CNCF Distribution

> **Status:** first full run 2026-04-23 (`make bench`, Apple Silicon,
> Docker Desktop 28.5.2, clean registries via `make dev-reset`).

## TL;DR

Both **Zot 2.1.2** and **CNCF Distribution 3.0.0** handle the L0 LOFS write
surface (`push_bucket` + `list_bucket_manifests` + `pull_bucket` +
`delete_bucket`) without issue. Custom media types
(`application/vnd.meteora.lofs.bucket.v1+json`), empty-layer artifact
manifests, `subject` (Referrers) links, and DELETE semantics are
compatible end-to-end.

**Headline numbers (median of the criterion sample):**

| Operation | Zot | Distribution | Winner |
|---|---:|---:|---|
| `create` single bucket | **18.2 ms** | **14.9 ms** | Distribution by ~18% |
| `stat` single bucket | **2.12 ms** | **0.62 ms** | Distribution by ~3.4× |
| `list` 1 bucket | **3.19 ms** | **1.45 ms** | Distribution by ~2.2× |
| `list` 10 buckets | **22.5 ms** | **8.3 ms** | Distribution by ~2.7× |
| 10 concurrent creates | **79 ms** | **46 ms** | Distribution by ~1.7× |

**Takeaway:** the reference CNCF Distribution implementation wins every
latency bucket we measured — unsurprising since it has the most
optimization effort behind it. Zot compensates with OCI 1.1 features
(native search, sync, cosign, zero-config delete) that matter for
production LOFS deployments. Pick Zot for feature richness, Distribution
for raw speed — our CI runs both.

Qualitative compatibility findings are in [Integration findings](#integration-findings);
weaknesses observed during implementation are tracked in
[Weaknesses by registry](#weaknesses-by-registry).

---

## Setup

| | Value |
|---|---|
| **Host** | macOS 15.x (Apple Silicon), Docker Desktop 28.5.2 |
| **Zot image** | `ghcr.io/project-zot/zot:v2.1.2` |
| **Distribution image** | `distribution/distribution:3.0.0` |
| **Network** | `docker compose` default bridge, published to `localhost` |
| **Ports** | Zot `5100`, Distribution `5101` |
| **Compose** | `docker/docker-compose.yml` |
| **Workload** | `crates/lofs-core/benches/registry_comparison.rs` (criterion, async via tokio) |
| **Runner** | `cargo bench -p lofs-core` (release profile, `lto = "thin"`, `codegen-units = 1`) |
| **Warmup** | criterion default — 3s warmup, 10s measurement per group |

Reproduce:

```bash
make dev-up        # zot + distribution up
make bench         # populates target/criterion/, prints ASCII table
```

---

## Latency results

Numbers are **criterion 95% CI** (`[lower  median  upper]`) in
milliseconds. Raw data: `target/criterion/<group>/<registry>/estimates.json`.
HTML reports: `target/criterion/report/index.html`.

### `create_single` — repeat push of the same bucket tag (20 samples)

| Registry | lower (ms) | **median (ms)** | upper (ms) |
|---|---:|---:|---:|
| Zot | 17.31 | **18.22** | 18.88 |
| Distribution | 14.57 | **14.89** | 15.27 |

Distribution is ~18% faster. Both include one `PUT` blob + one `PUT`
manifest round-trip over HTTP/1.1.

### `stat_single` — pull manifest + decode annotations (30 samples)

| Registry | lower (µs) | **median (µs)** | upper (µs) |
|---|---:|---:|---:|
| Zot | 2094 | **2120** | 2151 |
| Distribution | 607 | **620** | 634 |

Distribution is ~3.4× faster. The difference is in manifest read path —
Distribution caches manifests aggressively in-memory, Zot goes through
its blob-store abstraction every time.

### `list_scaling` — `/v2/_catalog` + per-bucket manifest pull (10 samples)

N is the number of pre-populated buckets. Time is a single
`list_buckets()` call that visits every one.

| Registry | N=1 (ms) | N=10 (ms) | N=10 per-bucket (ms) |
|---|---:|---:|---:|
| Zot | **3.19** | **22.52** | 2.25 |
| Distribution | **1.45** | **8.31** | 0.83 |

Both scale roughly linearly (expected — one catalog GET + one manifest
pull per repo). Distribution is ~2.5-2.7× faster across both N.

### `concurrent_create_10` — 10 pushes + 10 deletes in parallel (10 samples)

| Registry | lower (ms) | **median (ms)** | upper (ms) | throughput (elem/s) |
|---|---:|---:|---:|---:|
| Zot | 76.2 | **79.0** | 82.6 | 126.6 |
| Distribution | 44.3 | **45.97** | 47.6 | 217.5 |

Distribution roughly doubles throughput (218 vs 127 ops/sec). Under
parallel load the difference widens — Go's goroutine scheduler in
Distribution handles 10 concurrent request chains on one process more
smoothly than Zot's (also Go) request pipeline with its extra
storage-abstraction layer.

> **How to read:** lower latency is better; higher ops/sec is better.
> LOFS's `push_bucket` is a single-artifact, empty-layers operation — not
> representative of heavy image pushes. For heavy-layer pushes the numbers
> will look very different (blob upload dominates).

---

## Integration findings

Captured from the `tests/it_registry.rs` suite (`make test-e2e`). Each
assertion runs against both registries.

| Capability | Zot | Distribution |
|---|---|---|
| Push empty-layer manifest with custom media type | ✅ | ✅ |
| Manifest annotations round-trip | ✅ | ✅ |
| `/v2/_catalog` returns newly-created repo | ✅ | ✅ |
| `/v2/_catalog` includes stale empty repos after DELETE | ⚠️ yes, until GC | ❌ cleaned eagerly |
| Pull of unknown tag → `OciErrorCode::ManifestUnknown` | ⚠️ reports `NAME_UNKNOWN` | ✅ `MANIFEST_UNKNOWN` |
| DELETE without explicit enable flag | ✅ | ❌ needs `REGISTRY_STORAGE_DELETE_ENABLED=true` |
| Last-writer-wins on `:latest` tag update | ✅ | ✅ |
| Concurrent 10 pushes | ✅ | ✅ |
| Referrers API (`/v2/<repo>/referrers/<digest>`) — _L0+ coordination_ | ✅ (OCI 1.1) | ✅ (OCI 1.1) |

Notes:

- **Stale catalog entries after delete.** Zot leaves the repository stub
  present in `/v2/_catalog` until a GC sweep runs. Our `list_buckets()`
  tolerates this by skipping `NotFound` lookups — the e2e `delete_then_*`
  scenarios cover it directly.
- **Error code mapping.** When a repository has never seen a manifest,
  Zot returns `NAME_UNKNOWN` where Distribution returns
  `MANIFEST_UNKNOWN`. `is_manifest_not_found()` in `oci/registry.rs`
  matches both.
- **DELETE toggle.** Distribution refuses manifest DELETE by default
  (`405 Method Not Allowed`). The compose file sets
  `REGISTRY_STORAGE_DELETE_ENABLED=true` so the integration tests pass.

---

## Weaknesses by registry

### Zot

- **Catalog eventual consistency.** After a repo is deleted, it remains in
  the catalog response until the next GC cycle. Benign for us (we tolerate
  it), but noteworthy for anyone who uses the catalog as source-of-truth.
- **Non-standard `NAME_UNKNOWN`.** Zot returns `NAME_UNKNOWN` where the
  distribution spec permits either `NAME_UNKNOWN` _or_ `MANIFEST_UNKNOWN`
  for a GET on a tag that never existed. Clients that only match
  `MANIFEST_UNKNOWN` will break against Zot — be generous.
- **No built-in auth token issuer out of the box.** Our compose runs
  anonymous; production deploys need an external `htpasswd` / OIDC gateway.
- **Media-type allow-list edge case.** Zot accepts unknown manifest media
  types by default, but certain fields (e.g. `config.mediaType` with novel
  vendor prefixes) surface a warning in the server log even when the push
  succeeds. Not blocking.

### CNCF Distribution

- **DELETE disabled by default.** Any deploy that wants `lofs rm` working
  must set `REGISTRY_STORAGE_DELETE_ENABLED=true`; forget it once and
  every delete returns HTTP 405.
- **Filesystem driver is the only production-ready backend** shipped in
  the reference image. S3 / GCS / Azure drivers exist but require explicit
  config. Not a blocker for local dev / small-team self-host.
- **No Referrers API before 3.0.** Older deploys (2.x) that upgraded
  in-place may still answer Referrers with 404; Distribution 3.0.0 and
  newer are fine. L1+ coordination will feature-detect and fall back to
  tag-scan (`oci/referrers.rs`).

---

## Recommendation

- **Default for local dev / small teams:** **Zot**. Single binary, OCI 1.1
  native, extensions for search / sync / cosign, zero-config delete.
- **Baseline for production / interop testing:** **CNCF Distribution**.
  The reference implementation — if we pass its tests we pass every
  downstream registry (Harbor, GitLab Container Registry, GHCR — all
  fork or wrap it).
- **CI matrix:** run every release's e2e + bench suite against both, as
  the included `docker/docker-compose.yml` + `make test-e2e` + `make bench`
  already do.

---

## Running this benchmark yourself

```bash
# 1. start both registries
make dev-up

# 2. run the full integration matrix (must be green before benching)
make test-e2e

# 3. run the benchmarks — takes a few minutes for the full grid
make bench

# 4. tear down
make dev-down
```

Criterion HTML reports land in `target/criterion/report/index.html`.
