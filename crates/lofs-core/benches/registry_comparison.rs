//! Criterion benchmarks comparing Zot and CNCF Distribution for the core
//! LOFS registry operations: `create`, `list`, `stat`, `concurrent_create`.
//!
//! Both registries are expected to be running via `make dev-up`. URLs can
//! be overridden with `LOFS_BENCH_ZOT` / `LOFS_BENCH_DISTRIBUTION`.
//!
//! Run: `make bench` (forwards envs) or `cargo bench -p lofs-core`.
//!
//! ## Design note — "catalog pollution"
//!
//! Zot (and most OCI registries) GC repository catalog entries lazily,
//! so a naive "push + delete" iteration leaves hundreds of stub repos
//! behind after every sample. That poisons any subsequent benchmark
//! that walks `/v2/_catalog` (namely `list_scaling`).
//!
//! We work around this two ways:
//!
//! 1. `bench_create` and `bench_stat` reuse a **single** bucket name per
//!    registry. OCI `latest` is mutable, so a repeat push is a tag update
//!    (one repo, one blob, many manifest versions). We measure the cost
//!    of one push — not of repeatedly allocating fresh names.
//!
//! 2. `bench_list_scaling` provisions a throwaway org for each N, then
//!    deletes everything in bulk at the end of the run. The bench already
//!    runs on top of a `make dev-reset`-clean registry (see
//!    `bench/registry-comparison.md`).

use std::time::Duration;

use chrono::Utc;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use lofs_core::{Bucket, BucketName, NewBucket, OciRegistry};
use tokio::runtime::Runtime;
use uuid::Uuid;

/// URL used when `LOFS_BENCH_ZOT` is unset — matches `docker/docker-compose.yml`.
const DEFAULT_ZOT: &str = "http://localhost:5100";

/// URL used when `LOFS_BENCH_DISTRIBUTION` is unset — matches
/// `docker/docker-compose.yml`.
const DEFAULT_DISTRIBUTION: &str = "http://localhost:5101";

/// Build a multi-threaded tokio runtime.
///
/// We tried `new_current_thread` first; it deadlocked in `list_scaling`
/// after `create_single` completed — reqwest's connection pool appears to
/// keep tasks alive on the original runtime's executor, and when
/// criterion spins up a fresh runtime per benchmark closure the pool
/// connections are left in CLOSE_WAIT (observed via `netstat`). A
/// multi-thread runtime avoids the issue entirely.
fn runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn bench_org() -> String {
    let s = format!("bench-{}", Uuid::now_v7().simple());
    s.chars().take(30).collect()
}

/// Fresh bucket with unique name, useful for `list_scaling` seed and
/// `concurrent_create` where we need distinct names.
fn fresh_bucket(org: &str) -> Bucket {
    let name = format!("b-{}", Uuid::now_v7().simple());
    let name: String = name
        .chars()
        .take(30)
        .collect::<String>()
        .to_ascii_lowercase();
    NewBucket::try_new(name, Some(org.to_string()), 1, Some(64))
        .expect("valid bucket")
        .into_bucket_at(Utc::now())
}

/// Stable bucket for mutate-in-place benchmarks — same path on every push.
/// Returns a brand-new `Bucket` each call (different id + timestamp) so
/// the manifest body is never byte-identical.
fn stable_bucket(name: &str, org: &str) -> Bucket {
    NewBucket::try_new(name.to_string(), Some(org.to_string()), 1, Some(64))
        .expect("valid bucket")
        .into_bucket_at(Utc::now())
}

fn registries() -> Vec<(&'static str, String)> {
    vec![
        (
            "zot",
            std::env::var("LOFS_BENCH_ZOT").unwrap_or_else(|_| DEFAULT_ZOT.to_string()),
        ),
        (
            "distribution",
            std::env::var("LOFS_BENCH_DISTRIBUTION")
                .unwrap_or_else(|_| DEFAULT_DISTRIBUTION.to_string()),
        ),
    ]
}

fn ping_or_skip(reg: &OciRegistry) -> bool {
    let rt = runtime();
    rt.block_on(reg.ping()).is_ok()
}

// =====================================================================
// Suite 1: single `lofs.create` latency — push one tag update.
//
// Uses a **stable bucket name** so the registry doesn't collect stub
// repos across 20 samples. This measures "push a manifest" in isolation,
// not "allocate a new repo"; the difference is one catalog entry on the
// server side. See module docs for rationale.
// =====================================================================
fn bench_create(c: &mut Criterion) {
    let mut group = c.benchmark_group("create_single");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));

    for (label, url) in registries() {
        let reg = match OciRegistry::anonymous(&url) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{label}] skipping: {e}");
                continue;
            }
        };
        if !ping_or_skip(&reg) {
            eprintln!("[{label}] skipping: {url} unreachable");
            continue;
        }

        let org = bench_org();
        let bucket_name = format!("stable-{label}");

        group.bench_with_input(BenchmarkId::from_parameter(label), &reg, |b, reg| {
            let rt = runtime();
            b.to_async(&rt).iter(|| {
                let reg = reg.clone();
                let org = org.clone();
                let bucket_name = bucket_name.clone();
                async move {
                    let bucket = stable_bucket(&bucket_name, &org);
                    reg.push_bucket(&bucket).await.expect("push");
                }
            });
        });

        // Cleanup the single stable bucket when the group is done.
        let rt = runtime();
        let _ = rt.block_on(
            reg.delete_bucket(&BucketName::new(bucket_name.clone()).unwrap(), Some(&org)),
        );
    }
    group.finish();
}

// =====================================================================
// Suite 2: `lofs.list` latency with N pre-populated buckets in a
// dedicated org. Measures the per-bucket HTTP overhead (`/v2/_catalog`
// + per-repo manifest pull).
// =====================================================================
fn bench_list_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("list_scaling");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));

    for (label, url) in registries() {
        let reg = match OciRegistry::anonymous(&url) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{label}] skipping: {e}");
                continue;
            }
        };
        if !ping_or_skip(&reg) {
            eprintln!("[{label}] skipping: {url} unreachable");
            continue;
        }

        for &n in &[1_usize, 10_usize] {
            let rt = runtime();
            let org = bench_org();
            let seeded: Vec<(BucketName, String)> = rt.block_on(async {
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    let b = fresh_bucket(&org);
                    reg.push_bucket(&b).await.expect("seed push");
                    v.push((b.name.clone(), b.org.clone().unwrap_or_default()));
                }
                v
            });

            group.throughput(Throughput::Elements(n as u64));
            group.bench_with_input(BenchmarkId::new(label, format!("n={n}")), &reg, |b, reg| {
                let rt = runtime();
                b.to_async(&rt).iter(|| {
                    let reg = reg.clone();
                    async move {
                        let _ = reg.list_buckets().await.expect("list");
                    }
                });
            });

            // Bulk cleanup of everything we seeded for this N.
            rt.block_on(async {
                for (name, org) in &seeded {
                    let _ = reg.delete_bucket(name, Some(org)).await;
                }
            });
        }
    }
    group.finish();
}

// =====================================================================
// Suite 3: `lofs.stat` — pull a single manifest + decode annotations.
// =====================================================================
fn bench_stat(c: &mut Criterion) {
    let mut group = c.benchmark_group("stat_single");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(8));

    for (label, url) in registries() {
        let reg = match OciRegistry::anonymous(&url) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{label}] skipping: {e}");
                continue;
            }
        };
        if !ping_or_skip(&reg) {
            eprintln!("[{label}] skipping: {url} unreachable");
            continue;
        }

        let rt = runtime();
        let org = bench_org();
        let seeded = rt.block_on(async {
            let b = fresh_bucket(&org);
            reg.push_bucket(&b).await.expect("seed");
            b
        });
        let name = seeded.name.clone();
        let seeded_org = seeded.org.clone();

        group.bench_with_input(BenchmarkId::from_parameter(label), &reg, |b, reg| {
            let rt = runtime();
            b.to_async(&rt).iter(|| {
                let reg = reg.clone();
                let name = name.clone();
                let org = seeded_org.clone();
                async move {
                    let _ = reg.pull_bucket(&name, org.as_deref()).await.expect("stat");
                }
            });
        });

        rt.block_on(async {
            let _ = reg.delete_bucket(&name, seeded_org.as_deref()).await;
        });
    }
    group.finish();
}

// =====================================================================
// Suite 4: concurrent create throughput — 10 pushes in parallel.
//
// Unlike `bench_create` we DO want fresh names here (measuring parallel
// blob/manifest upload). Sample count stays tight so cumulative stub
// repo count stays manageable.
// =====================================================================
fn bench_concurrent_create(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_create_10");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    for (label, url) in registries() {
        let reg = match OciRegistry::anonymous(&url) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{label}] skipping: {e}");
                continue;
            }
        };
        if !ping_or_skip(&reg) {
            eprintln!("[{label}] skipping: {url} unreachable");
            continue;
        }

        group.throughput(Throughput::Elements(10));
        group.bench_with_input(BenchmarkId::from_parameter(label), &reg, |b, reg| {
            let rt = runtime();
            b.to_async(&rt).iter(|| {
                let reg = reg.clone();
                async move {
                    let org = bench_org();
                    let mut handles = Vec::with_capacity(10);
                    for _ in 0..10 {
                        let reg = reg.clone();
                        let org = org.clone();
                        handles.push(tokio::spawn(async move {
                            let b = fresh_bucket(&org);
                            reg.push_bucket(&b).await.expect("push");
                            (b.name, b.org)
                        }));
                    }
                    let created = futures::future::join_all(handles).await;
                    for (n, o) in created.into_iter().flatten() {
                        let _ = reg.delete_bucket(&n, o.as_deref()).await;
                    }
                }
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_create,
    bench_list_scaling,
    bench_stat,
    bench_concurrent_create
);
criterion_main!(benches);
