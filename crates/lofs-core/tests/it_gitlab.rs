//! Live GitLab.com Container Registry integration test.
//!
//! Opt-in: set these env vars to run it (otherwise every test case
//! short-circuits to a skip message):
//!
//! ```text
//!   LOFS_GITLAB_URL=https://registry.gitlab.com/<user>/<project>
//!   LOFS_GITLAB_USERNAME=<gitlab-username>
//!   LOFS_GITLAB_TOKEN=glpat-<classic-or-fine-grained-pat>
//! ```
//!
//! For `rm` the PAT must also carry the `api` scope (classic) or the
//! "API" granular permission (fine-grained) on top of `read_registry` +
//! `write_registry`. `create` / `list` / `stat` work with registry-only
//! scopes.
//!
//! Invoke with `cargo test -p lofs-core --test it_gitlab -- --ignored`.
//! We keep the test `#[ignore]` so CI doesn't pull credentials unless
//! the runner explicitly provides them (GitHub Actions: a secret-gated
//! job can `unset-if-empty` the env vars before running).
//!
//! Bucket names are randomised per run, and the test cleans up its own
//! fixtures — re-running it against the same project is safe.

use std::env;

use chrono::Utc;
use lofs_core::bucket::BucketStatus;
use lofs_core::{BucketName, NewBucket, OciRegistry};

fn gitlab_registry() -> Option<(String, String, String)> {
    let url = env::var("LOFS_GITLAB_URL").ok()?;
    let user = env::var("LOFS_GITLAB_USERNAME").ok()?;
    let token = env::var("LOFS_GITLAB_TOKEN").ok()?;
    Some((url, user, token))
}

fn skip_if_no_creds() -> Option<(OciRegistry, &'static str)> {
    let (url, user, token) = gitlab_registry()?;
    let reg = OciRegistry::anonymous(&url)
        .expect("valid LOFS_GITLAB_URL")
        .with_basic(user, token);
    assert_eq!(
        reg.driver().name(),
        "gitlab",
        "auto-detect picked wrong driver for {url}"
    );
    Some((reg, "gitlab-live"))
}

fn unique_name(prefix: &str) -> String {
    let uuid = uuid::Uuid::now_v7().simple().to_string();
    let short = &uuid[..12];
    format!("{prefix}-{short}").to_ascii_lowercase()
}

#[tokio::test]
#[ignore = "requires LOFS_GITLAB_{URL,USERNAME,TOKEN} env"]
async fn gitlab_create_list_stat_roundtrip() {
    let Some((reg, label)) = skip_if_no_creds() else {
        eprintln!("skipping: LOFS_GITLAB_* env not set");
        return;
    };
    reg.ping().await.expect("ping");

    let name_raw = unique_name("it-glab");
    let name = BucketName::new(name_raw.clone()).unwrap();
    let bucket = NewBucket::try_new(name_raw.clone(), Some("it".into()), 1, Some(32))
        .unwrap()
        .into_bucket_at(Utc::now());

    reg.push_bucket(&bucket).await.expect("create");
    let pulled = reg.pull_bucket(&name, Some("it")).await.expect("stat");
    assert_eq!(pulled.id, bucket.id, "[{label}] id roundtrip");
    assert_eq!(pulled.status, BucketStatus::Active);

    let listed = reg.list_buckets().await.expect("list");
    assert!(
        listed.iter().any(|b| b.id == bucket.id),
        "[{label}] created bucket missing from list"
    );

    // Best-effort cleanup. If the PAT doesn't have `api` scope the DELETE
    // fails — that's fine; log it but don't panic, we don't want this
    // test to block releases when the caller used a reduced-scope token.
    match reg.delete_bucket(&name, Some("it")).await {
        Ok(()) => {}
        Err(e) => eprintln!("[{label}] cleanup failed (token may lack api scope): {e}"),
    }
}

#[tokio::test]
#[ignore = "requires LOFS_GITLAB_{URL,USERNAME,TOKEN} env + `api` PAT scope"]
async fn gitlab_rm_via_api() {
    let Some((reg, label)) = skip_if_no_creds() else {
        eprintln!("skipping: LOFS_GITLAB_* env not set");
        return;
    };

    let name_raw = unique_name("it-glab-rm");
    let name = BucketName::new(name_raw.clone()).unwrap();
    let bucket = NewBucket::try_new(name_raw, None, 1, Some(16))
        .unwrap()
        .into_bucket_at(Utc::now());
    reg.push_bucket(&bucket).await.expect("create");

    reg.delete_bucket(&name, None)
        .await
        .expect("rm via GitLab API — requires `api` scope PAT");

    // After DELETE the bucket should not appear in list anymore.
    let listed = reg.list_buckets().await.expect("post-rm list");
    assert!(
        !listed.iter().any(|b| b.id == bucket.id),
        "[{label}] deleted bucket still visible"
    );
}
