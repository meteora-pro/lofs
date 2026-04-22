---
id: RESEARCH-005
title: Rust ecosystem для Docker/OCI/container infrastructure 2025-2026
date: 2026-04-22
tags: ["research", "rust", "oci", "docker", "containerd", "buildah", "fuse", "rootless"]
related_adrs: ["ADR-001"]
---

# RESEARCH-005: Rust ecosystem для OCI/container infrastructure

> Production-focused research для lofs-daemon — что можно переиспользовать vs писать самим.

---

## Executive summary

- **Спецификации и клиенты OCI — зрелые в Rust**: `oci-spec`, `oci-client`, `bollard`, `containerd-client`, `ttrpc-rust` — production.
- **Youki и bottlerocket — доказательство** что Rust container runtime scales до CNCF/AWS.
- **Critical gap**: нет Rust port `containers/storage` (Go — shared между Buildah/Podman/CRI-O). **libfuse-fs + ocirender** — лучший beta-tier бет.
- **`ocirender` (edera-dev) — прорывной find 2025**: streaming OCI layer merge с correct whiteouts, может снять 2000-3500 LOC.
- **`zstd:chunked` writer — дыра**. Либо писать (~800 LOC), либо CLI exec `podman image convert`.
- **Критичный риск**: CVE-2025-62518 (TARmageddon) в `tokio-tar`. Использовать только `tar-rs` или custom.
- **Cross-platform**: **Linux first — единственный реалистичный MVP**. macOS через libkrun VM, Windows через WSL2 — Phase 2+.

---

## Top-15 Rust crates/projects (ranked)

| # | Crate | Maturity | Что даёт |
|---|-------|----------|----------|
| 1 | [`oci-client`](https://github.com/oras-project/rust-oci-client) | Production | OCI Distribution client (push/pull/auth/referrers) |
| 2 | [`oci-spec`](https://github.com/youki-dev/oci-spec-rs) | Production | Canonical OCI types (Image/Runtime/Distribution) |
| 3 | [`ocirender`](https://edera.dev/stories/rendering-oci-images-the-right-way-introducing-ocirender) | Beta — **прорыв** | Streaming OCI layer merge → squashfs/tar/directory, whiteout handling |
| 4 | [`libfuse-fs`](https://crates.io/crates/libfuse-fs) | Beta | OverlayFS + whiteouts в userspace, rootless-ready |
| 5 | [`fuser`](https://github.com/cberner/fuser) | Production | Canonical FUSE crate, libfuse 3.10 API |
| 6 | [`fuse3`](https://github.com/Sherlock-Holo/fuse3) | Production | Async FUSE для tokio |
| 7 | [`bollard`](https://github.com/fussybeaver/bollard) | Production | Docker Engine + Podman API client |
| 8 | [`containerd-client`](https://crates.io/crates/containerd-client) + [`ttrpc-rust`](https://github.com/containerd/ttrpc-rust) | Beta + Production | gRPC/ttrpc к containerd |
| 9 | [`tar-rs`](https://github.com/alexcrichton/tar-rs) | Production | Tar read/write (НЕ tokio-tar — CVE) |
| 10 | [`zstd`](https://github.com/gyscos/zstd-rs) + [`zstd-framed`](https://github.com/kylewlacy/zstd-framed) + [`async-compression`](https://docs.rs/async-compression) | Production | Compression ecosystem |
| 11 | [`sys-mount`](https://github.com/pop-os/sys-mount) | Production | Mount/umount syscall wrapper, builder pattern |
| 12 | [`caps`](https://github.com/lucab/caps-rs) + `nix` + [`unshare`](https://github.com/tailhook/unshare) | Production | Linux capabilities + namespaces |
| 13 | [`iroh-blobs`](https://github.com/n0-computer/iroh-blobs) | Production | BLAKE3 CAS, optional backend |
| 14 | [`nydus-storage`](https://github.com/dragonflyoss/nydus) | Production | Chunked image storage patterns |
| 15 | [`container_registry-rs`](https://github.com/mbr/container_registry-rs) | Beta | Self-hosted tiny registry как axum app |

## Abandoned / deprecated — не использовать

- **`shiplift`** — 4+ года без коммитов. Путь: `bollard`.
- **`dkregistry-rs`** — legacy. Путь: `oci-client`.
- **`krustlet`** — dead since 2022. Путь: `containerd-runwasi`.
- **`tokio-tar` (любой fork)** — CVE-2025-62518. Путь: `tar-rs` + `spawn_blocking`.

## Critical gaps (нет production Rust)

1. **Rust port of `containers/storage`** — ~5-10k LOC если pure Rust.
2. **zstd:chunked writer** — ~800 LOC самим или CLI exec Podman.
3. **Buildah Rust FFI** — только CLI exec (`std::process::Command`).
4. **SOCI zTOC Rust reader** — только Go AWS SDK.
5. **Rust-native BuildKit alternative** — пусто.
6. **Rust-native Buildah** — пусто.

## License compatibility

Всё mainstream — **Apache 2.0 / MIT dual-licensed**, compatible с нашим Apache 2.0. Особенные точки:

- **Mergiraf** (GPL-3.0) — subprocess only (exec boundary), не static-link.
- **MinIO** (AGPL-3.0) — как external service не contaminate; embed запрещён.
- **libfuse** (LGPL 2.1) — dynamic link ok; fuser имеет pure-Rust fallback.

## Production adopters как доказательство жизнеспособности

- **Fermyon Spin** — OCI artifacts + `oci-client`
- **wasmCloud (CNCF Graduated)** — Rust-first, cosign signing
- **AWS Bottlerocket** — Rust OS для containers
- **Red Hat libkrun/netavark/aardvark-dns** — Rust для Podman ecosystem
- **ByteDance/Alibaba Nydus** — миллионы контейнеров/день с RAFS
- **Cloudflare workerd + workers-rs** — edge runtime
- **Microsoft Hyperlight** — Rust micro-VM (0.0009s execution)
- **Firecracker + cloud-hypervisor** — AWS Lambda, Kata, Northflank

## Architecture recommendation для lofs-daemon

### Dependency tree

```toml
[dependencies]
# OCI spec + registry
oci-spec = "0.8"
oci-client = "=0.16.1"            # pin pre-1.0
oci-tar-builder = "0.2"           # optional packaging patterns

# FUSE + rootless mount
fuser = "0.17"                    # sync
fuse3 = "1.91"                    # async для tokio
libfuse-fs = "0.3"                # overlay+whiteouts (beta, pin strict!)

# Storage + chunking
iroh-blobs = "0.35"               # optional CAS
fastcdc = "3"
blake3 = "1"
ciborium = "0.2"

# Compression
zstd = "0.13"
async-compression = { version = "0.4", features = ["zstd", "tokio"] }
zstd-framed = "0.1"               # seekable zstd

# Tar — BE CAREFUL
tar = "0.4"                       # sync tar-rs; НЕ tokio-tar

# Low-level Linux
nix = { version = "0.28", features = ["mount", "sched", "user"] }
caps = "0.5"
sys-mount = "3"
unshare = "0.7"

# Docker/Podman (optional integration)
bollard = "0.19"
# OR containerd:
# containerd-client = "0.8"
# ttrpc = "0.8"

# Storage abstraction
opendal = "0.55"
moka = "0.12"

# Signing
ed25519-dalek = "2"
# sigstore = "0.12"               # experimental — prefer cosign CLI для MVP
```

### Что через CLI exec

| CLI | Зачем |
|-----|-------|
| `buildah` | Rootless layer commit когда libfuse-fs недостаточен |
| `podman image convert --compression=zstd:chunked` | Pure Rust writer отсутствует |
| `cosign` | Signing (sigstore-rs pre-1.0) |
| `skopeo` | Cross-registry copy |
| `mksquashfs` | squashfs output (если выбрано) |

### Что писать самим (LOC estimate)

| Module | LOC | Rationale |
|--------|-----|-----------|
| OCI mediatype + manifest builder (`vnd.meteora.lofs.*`) | 400-600 | Custom subject-refs + Referrers API wiring |
| Pack-file writer (zstd:chunked) | 800-1200 | Нет production Rust impl |
| Pack-file reader с Range-GET (SOCI-style) | 600-900 | Custom zTOC parser |
| Tree canonical CBOR + BLAKE3 merkle | 300-500 | |
| Snapshot DAG + lookup + GC | 1500-2500 | Postgres + Redis metadata |
| Overlayfs commit → tar layer | 300-600 | libfuse-fs даёт mount, но commit наш |
| Rootless mount orchestration | 400-700 | unshare + libfuse-fs assembly |
| Intent state machine + heartbeat | 500-1000 | 7-state lifecycle |
| Merge engine 4-tier ladder | 1500-2500 | Mergiraf subprocess + fallbacks |
| MCP tool handlers (rmcp) | 2000-3000 | Tool schemas + handlers |
| Buildah CLI wrapper | 200-400 | Thin Command exec |
| **Total** | **~8500-14000 LOC** | **−30-50% если ocirender дозревает** |

## Alternatives paths (если Rust ecosystem gap)

### Option A — Go CLI wrapping (fastest MVP)
- `skopeo`, `buildah`, `podman`, `cosign`, `oras` через `std::process::Command`.
- **+** самый быстрый путь, всё production
- **−** fork overhead, dependencies в Docker image daemon'а

### Option B — FFI к `containers/storage.so`
- Go `-buildmode=c-shared` → Rust FFI.
- **+** реюз battle-tested Go
- **−** boilerplate, Go GC в Rust process непредсказуем
- Не для MVP; возможно Phase 8.

### Option C — Hybrid: Rust core + youki libcontainer primitives
- Namespace/mount helpers из `youki::libcontainer`.
- **+** всё Rust, Apache 2.0
- **−** youki — runtime focus, не layer storage

### Option D — Kernel overlayfs (не rootless!)
- `sys-mount` wrap, требует `CAP_SYS_ADMIN`.
- **+** минимум Rust (~500 LOC)
- **−** меняет deployment story (daemon должен быть privileged)

---

## Risks

### API instability
- **oci-client** — pre-1.0, breaking между minor. Pin `=0.16.1`.
- **ocirender** — beta. Pin + закрывать API за наш trait.
- **libfuse-fs** — beta, single maintainer. Возможно fork если long-term.
- **sigstore-rs** — pre-1.0. Для production — cosign CLI.
- **zstd-chunked** — pre-alpha. Не полагаться.

### Cross-platform
- overlayfs (kernel + fuse-overlayfs) — Linux only.
- sys-mount, libfuse-fs — Linux primary.
- macOS: FUSE через macFUSE kext или libkrun VM (Linux guest).
- Windows: WSL2 реалистично; native — сложно.

### Specific
- **Rootless FUSE** требует user namespace. Detection at startup + fallback.
- **zstd:chunked writer dependency** — решаем через CLI exec.
- **Buildah via exec** — fork overhead на каждый mount/commit (~50-200ms).

---

## Ключевые выводы

1. **oci-spec + oci-client + bollard — rock-solid база**. Берём без сомнений.
2. **libfuse-fs + ocirender — критичный путь для rootless mount**. Beta-tier, мониторим закрытия.
3. **ocirender может снять 30-50% LOC** если directory-output mode напрямую подходит. Needs spike.
4. **Избегать tokio-tar полностью** — TARmageddon уяснил почему.
5. **zstd:chunked writer, SOCI Rust reader, containers/storage port — всё пусто**. Либо ждём (watch), либо CLI exec, либо пишем сами.
6. **Our custom LOC — ~8500-14000**, минус 30-50% если beta crates дозреют. Не 30k как в original ADR estimate.
7. **Linux-first MVP — единственный реалистичный путь**. macOS/Windows — Phase 2+.
8. **Licensing clean** — ecosystem весь Apache 2.0 / MIT.

---

## Watch-list (чекать раз в квартал)

- **ocirender** stabilization → production release
- **libfuse-fs** — дополнительные maintainers / v1.0
- **sigstore-rs** → 1.0 (тогда internal signing полностью в Rust)
- **zstd:chunked** Rust writer crate появится?
- **SOCI Rust reader** — кто-то напишет?
- **Rust port of `containers/storage`** — вероятность низкая, но watch
- **Cloudflare Containers OSS** — если что-то из их infra выйдет OSS
- **Edera ecosystem** — styrolite, ocirender, cve-tarmageddon — всё в том же org
