//! End-to-end integration tests for `OciRegistry` against live registries.
//!
//! Every test runs twice — once against Zot (via `LOFS_TEST_ZOT` / default
//! `http://localhost:5100`) and once against CNCF Distribution (via
//! `LOFS_TEST_DISTRIBUTION` / `http://localhost:5101`) — so any
//! compatibility drift between the two implementations shows up in the
//! test output.
//!
//! All tests are marked `#[ignore]` so a normal `cargo test` run stays
//! hermetic. Invoke via `make test-e2e` (adds `--include-ignored`) or
//! `cargo test -- --ignored`.
//!
//! Prereq: `make dev-up` must have been run first.

mod common;

use chrono::Utc;
use common::{RegistryUnderTest, cleanup_bucket, create_fresh_bucket, unique_name, unique_org};
use lofs_core::bucket::BucketStatus;
use lofs_core::error::LofsError;
use lofs_core::{BucketName, NewBucket};

// =====================================================================
// Suite 1: basic CRUD — create / stat / list / rm happy path.
// =====================================================================

async fn scenario_create_roundtrip(reg: &RegistryUnderTest) {
    reg.require_reachable().await;
    let client = reg.client();
    let bucket = create_fresh_bucket(&client, "crud", Some("it-test"), 1).await;

    let pulled = client
        .pull_bucket(&bucket.name, Some("it-test"))
        .await
        .expect("pull_bucket");
    assert_eq!(pulled.id, bucket.id, "[{}] id roundtrip", reg.label);
    assert_eq!(pulled.name.as_str(), bucket.name.as_str());
    assert_eq!(pulled.org.as_deref(), Some("it-test"));
    assert_eq!(pulled.ttl_days, 1);
    assert_eq!(pulled.status, BucketStatus::Active);

    cleanup_bucket(&client, &bucket.name, Some("it-test")).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn create_roundtrip_zot() {
    scenario_create_roundtrip(&RegistryUnderTest::zot()).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn create_roundtrip_distribution() {
    scenario_create_roundtrip(&RegistryUnderTest::distribution()).await;
}

// =====================================================================
// Suite 2: list sees what we push, filters the rest.
// =====================================================================

async fn scenario_list_returns_created(reg: &RegistryUnderTest) {
    reg.require_reachable().await;
    let client = reg.client();
    let org = unique_org();
    let b1 = create_fresh_bucket(&client, "list", Some(&org), 3).await;
    let b2 = create_fresh_bucket(&client, "list", Some(&org), 3).await;

    let all = client.list_buckets().await.expect("list");
    let mine: Vec<_> = all
        .into_iter()
        .filter(|b| b.org.as_deref() == Some(&org))
        .collect();
    assert_eq!(
        mine.len(),
        2,
        "[{}] expected 2 buckets in org {org}, got {}",
        reg.label,
        mine.len()
    );
    let ids: std::collections::HashSet<_> = mine.iter().map(|b| b.id).collect();
    assert!(ids.contains(&b1.id));
    assert!(ids.contains(&b2.id));

    cleanup_bucket(&client, &b1.name, Some(&org)).await;
    cleanup_bucket(&client, &b2.name, Some(&org)).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn list_returns_created_zot() {
    scenario_list_returns_created(&RegistryUnderTest::zot()).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn list_returns_created_distribution() {
    scenario_list_returns_created(&RegistryUnderTest::distribution()).await;
}

// =====================================================================
// Suite 3: rm deletes, then pull returns NotFound.
// =====================================================================

async fn scenario_delete_then_not_found(reg: &RegistryUnderTest) {
    reg.require_reachable().await;
    let client = reg.client();
    let name_raw = unique_name("rm-then-404");
    let name = BucketName::new(name_raw.clone()).unwrap();
    let bucket = NewBucket::try_new(name_raw, None, 1, None)
        .unwrap()
        .into_bucket_at(Utc::now());
    client.push_bucket(&bucket).await.unwrap();
    client.delete_bucket(&name, None).await.expect("delete");

    let err = client
        .pull_bucket(&name, None)
        .await
        .expect_err("expected NotFound after delete");
    assert!(
        matches!(err, LofsError::NotFound(_)),
        "[{}] expected NotFound, got {err:?}",
        reg.label
    );
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn delete_then_not_found_zot() {
    scenario_delete_then_not_found(&RegistryUnderTest::zot()).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn delete_then_not_found_distribution() {
    scenario_delete_then_not_found(&RegistryUnderTest::distribution()).await;
}

// =====================================================================
// Suite 4: pull of unknown name returns NotFound (not opaque Registry error).
// =====================================================================

async fn scenario_pull_unknown_is_not_found(reg: &RegistryUnderTest) {
    reg.require_reachable().await;
    let client = reg.client();
    let phantom = BucketName::new(unique_name("phantom")).unwrap();
    let err = client
        .pull_bucket(&phantom, Some("ghost"))
        .await
        .expect_err("should be NotFound");
    assert!(
        matches!(err, LofsError::NotFound(_)),
        "[{}] expected NotFound, got {err:?}",
        reg.label
    );
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn pull_unknown_is_not_found_zot() {
    scenario_pull_unknown_is_not_found(&RegistryUnderTest::zot()).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn pull_unknown_is_not_found_distribution() {
    scenario_pull_unknown_is_not_found(&RegistryUnderTest::distribution()).await;
}

// =====================================================================
// Suite 5: the artifact we push reports our custom media type.
// =====================================================================

async fn scenario_custom_media_type_survives(reg: &RegistryUnderTest) {
    reg.require_reachable().await;
    let client = reg.client();
    let bucket = create_fresh_bucket(&client, "mt", Some("it-mt"), 1).await;

    let manifests = client
        .list_bucket_manifests()
        .await
        .expect("list_manifests");
    let ours = manifests
        .iter()
        .find(|m| m.tag_ref.contains(bucket.name.as_str()))
        .unwrap_or_else(|| panic!("[{}] manifest for {} not listed", reg.label, bucket.name));
    assert!(
        ours.is_bucket(),
        "[{}] manifest missing pro.meteora.lofs.kind=bucket annotation",
        reg.label
    );
    assert!(
        ours.digest.starts_with("sha256:"),
        "[{}] expected sha256 digest, got {}",
        reg.label,
        ours.digest
    );

    cleanup_bucket(&client, &bucket.name, Some("it-mt")).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn custom_media_type_survives_zot() {
    scenario_custom_media_type_survives(&RegistryUnderTest::zot()).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn custom_media_type_survives_distribution() {
    scenario_custom_media_type_survives(&RegistryUnderTest::distribution()).await;
}

// =====================================================================
// Suite 6: duplicate push — last-writer-wins semantics (ADR-002 §L0).
// =====================================================================

async fn scenario_duplicate_push_is_last_writer_wins(reg: &RegistryUnderTest) {
    reg.require_reachable().await;
    let client = reg.client();
    let name_raw = unique_name("lww");
    let name = BucketName::new(name_raw.clone()).unwrap();

    let first = NewBucket::try_new(name_raw.clone(), None, 1, Some(100))
        .unwrap()
        .into_bucket_at(Utc::now());
    client.push_bucket(&first).await.unwrap();

    let second = NewBucket::try_new(name_raw, None, 7, Some(999))
        .unwrap()
        .into_bucket_at(Utc::now());
    client.push_bucket(&second).await.unwrap();

    let pulled = client.pull_bucket(&name, None).await.unwrap();
    assert_eq!(
        pulled.id, second.id,
        "[{}] expected second push to win :latest",
        reg.label
    );
    assert_eq!(pulled.ttl_days, 7);
    assert_eq!(pulled.size_limit_mb, 999);

    cleanup_bucket(&client, &name, None).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn duplicate_push_is_last_writer_wins_zot() {
    scenario_duplicate_push_is_last_writer_wins(&RegistryUnderTest::zot()).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn duplicate_push_is_last_writer_wins_distribution() {
    scenario_duplicate_push_is_last_writer_wins(&RegistryUnderTest::distribution()).await;
}

// =====================================================================
// Suite 7: 10 concurrent creates — exercises the blob-upload machinery
// and makes sure we don't serialise accidentally.
// =====================================================================

async fn scenario_concurrent_creates(reg: &RegistryUnderTest) {
    reg.require_reachable().await;
    let client = reg.client();
    let org = unique_org();

    let futures: Vec<_> = (0..10)
        .map(|i| {
            let client = client.clone();
            let org = org.clone();
            tokio::spawn(async move {
                let name = unique_name(&format!("conc-{i:02}"));
                let bucket = NewBucket::try_new(name.clone(), Some(org.clone()), 1, Some(128))
                    .unwrap()
                    .into_bucket_at(Utc::now());
                client.push_bucket(&bucket).await.unwrap();
                BucketName::new(name).unwrap()
            })
        })
        .collect();

    let names: Vec<_> = futures::future::join_all(futures)
        .await
        .into_iter()
        .map(|r| r.expect("task"))
        .collect();

    let listed = client.list_buckets().await.expect("list");
    let count = listed
        .iter()
        .filter(|b| b.org.as_deref() == Some(&org))
        .count();
    assert_eq!(
        count, 10,
        "[{}] expected 10 buckets in org {org}, saw {count}",
        reg.label
    );

    // Clean up serially — delete_bucket needs a round-trip HEAD+DELETE per
    // bucket, and parallel deletes on Zot slow it down further.
    for n in &names {
        cleanup_bucket(&client, n, Some(&org)).await;
    }
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn concurrent_creates_zot() {
    scenario_concurrent_creates(&RegistryUnderTest::zot()).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn concurrent_creates_distribution() {
    scenario_concurrent_creates(&RegistryUnderTest::distribution()).await;
}

// =====================================================================
// Suite 8: "personal" org segment is reserved at the registry path level.
// =====================================================================

async fn scenario_personal_org_is_reserved(reg: &RegistryUnderTest) {
    reg.require_reachable().await;
    let client = reg.client();
    let name = unique_name("reserve");
    let bucket = NewBucket::try_new(name, Some("personal".into()), 1, None)
        .unwrap()
        .into_bucket_at(Utc::now());
    let err = client
        .push_bucket(&bucket)
        .await
        .expect_err("expected reserved org rejection");
    match err {
        LofsError::InvalidName { name, reason } => {
            assert_eq!(name, "personal");
            assert!(reason.contains("reserved"));
        }
        other => panic!("[{}] expected InvalidName, got {other:?}", reg.label),
    }
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn personal_org_is_reserved_zot() {
    scenario_personal_org_is_reserved(&RegistryUnderTest::zot()).await;
}

#[tokio::test]
#[ignore = "requires `make dev-up`"]
async fn personal_org_is_reserved_distribution() {
    scenario_personal_org_is_reserved(&RegistryUnderTest::distribution()).await;
}
