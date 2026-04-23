//! `GitLabDriver` — GitLab Container Registry quirks.
//!
//! Observed behaviour on `registry.gitlab.com`:
//!
//! - **Path is project-scoped.** A PAT can authenticate for
//!   `repository:<user>/<project>:pull,push` but nothing below. Pushes to
//!   `<user>/<project>/lofs/<org>/<name>` will succeed in blob upload
//!   (thanks to the project-root JWT) but the manifest PUT is rejected
//!   because the tag write is under a sub-path the JWT didn't claim. We
//!   force [`RepoMode::Shared`] so every bucket lives on the project repo
//!   itself, identified by tag.
//!
//! - **`artifactType` is on an allow-list.** Pushing a manifest with
//!   `artifactType: application/vnd.meteora.lofs.bucket.v1+json` returns
//!   `MANIFEST_INVALID: unknown media type`. Disable.
//!
//! - **`DELETE /v2/.../manifests/<digest>` is closed.** On the hosted
//!   service it 404s ("404 page not found" from the reverse proxy). We
//!   dispatch DELETEs via GitLab's native REST API instead —
//!   `DELETE /api/v4/projects/:id/registry/repositories/:rid/tags/:tag` —
//!   after a one-time `(path → project_id → repo_id)` resolve we cache
//!   for the lifetime of the driver.
//!
//! Rate limit: GitLab enforces ~2000 authenticated API requests/minute.
//! We clamp to a moderate concurrency to leave headroom for the user's
//! other GitLab tools (git push, CI).

pub mod api;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use oci_client::secrets::RegistryAuth;
use tokio::sync::RwLock;

use super::super::registry::RepoMode;
use super::RateLimitPolicy;
use super::{DeleteContext, RegistryDriver};
use crate::error::{LofsError, LofsResult};
use api::{ApiAuth, GitLabApi};

/// GitLab Container Registry driver.
///
/// Holds a small shared cache mapping the project prefix to the GitLab
/// numeric `project_id` + `repository_id`, so the DELETE path only pays
/// for the lookup round-trips on the first call per session.
#[derive(Clone, Debug, Default)]
pub struct GitLabDriver {
    cache: Arc<RwLock<Option<ProjectCache>>>,
}

#[derive(Clone, Debug)]
struct ProjectCache {
    project_path: String, // e.g. "andreymaznyak/lofs-testbed"
    project_id: u64,
    repo_id: u64,
}

impl GitLabDriver {
    /// Build a fresh driver with an empty resolve-cache.
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Resolve (and cache) the GitLab numeric id pair for our path prefix.
    async fn resolve(&self, api: &GitLabApi, project_path: &str) -> LofsResult<(u64, u64)> {
        {
            let cache = self.cache.read().await;
            if let Some(c) = cache.as_ref()
                && c.project_path == project_path
            {
                return Ok((c.project_id, c.repo_id));
            }
        }

        let project = api.get_project(project_path).await?;
        let repos = api.list_registry_repositories(project.id).await?;
        // LOFS always pushes to the project's default repo (Shared mode
        // → one repo per project). Match by exact path; if the user
        // somehow ends up with multiple sub-repos, prefer the one whose
        // path equals the project prefix exactly.
        let repo = repos
            .iter()
            .find(|r| r.path == project_path)
            .or_else(|| repos.first())
            .ok_or_else(|| {
                LofsError::NotFound(format!(
                    "no registry repository under project `{project_path}` — \
                     push a manifest first, or check project UI"
                ))
            })?;

        let ids = (project.id, repo.id);
        let mut cache = self.cache.write().await;
        *cache = Some(ProjectCache {
            project_path: project_path.to_string(),
            project_id: ids.0,
            repo_id: ids.1,
        });
        Ok(ids)
    }
}

#[async_trait]
impl RegistryDriver for GitLabDriver {
    fn name(&self) -> &'static str {
        "gitlab"
    }

    fn description(&self) -> &'static str {
        "GitLab Container Registry — Shared repo mode, DELETE via /api/v4"
    }

    /// GitLab requires Shared mode — see module docs. If somebody points
    /// the CLI at a bare-host GitLab (`--registry https://registry.gitlab.com`)
    /// without a project prefix, they'd hit 401 on any write; fall back to
    /// Shared anyway so at least `doctor` and `list` degrade gracefully.
    fn effective_repo_mode(&self, _path_prefix: &str) -> RepoMode {
        RepoMode::Shared
    }

    fn supports_artifact_type(&self) -> bool {
        false
    }

    /// Native OCI DELETE is closed on managed GitLab, but we do have a
    /// functional delete path — via the GitLab REST API, invoked by
    /// `delete_manifest`.
    fn supports_native_delete(&self) -> bool {
        false
    }

    fn rate_limit_policy(&self) -> RateLimitPolicy {
        RateLimitPolicy {
            max_concurrent: Some(10),
            retry_after_header: "Retry-After",
            default_backoff: Duration::from_secs(2),
            max_retries: 5,
        }
    }

    async fn delete_manifest<'a>(
        &self,
        ctx: &'a DeleteContext<'a>,
    ) -> LofsResult<reqwest::Response> {
        if ctx.path_prefix.is_empty() {
            return Err(LofsError::Registry(
                "GitLab DELETE needs a project path prefix in --registry URL \
                 (e.g. https://registry.gitlab.com/<user>/<project>)"
                    .into(),
            ));
        }

        let registry_host = ctx.reference.registry().to_string();
        let api_base = GitLabApi::derive_from_registry_host(&registry_host);
        let api_auth = api_auth_from(ctx.auth)?;
        let api = GitLabApi::new(api_base, api_auth);

        let (project_id, repo_id) = self.resolve(&api, ctx.path_prefix).await?;

        // The tag is the part of the Reference after the colon — this is
        // what we originally encoded from (org, name) in Shared mode.
        let tag = ctx
            .reference
            .tag()
            .ok_or_else(|| LofsError::Registry("reference missing tag for DELETE".into()))?
            .to_string();

        let status = api.delete_registry_tag(project_id, repo_id, &tag).await?;

        // Synthesize a minimal `reqwest::Response` shape is painful;
        // instead we bypass it by returning a canned-OK response via a
        // dummy 200 loopback. The caller only checks `.status()`.
        synth_response(status)
    }
}

fn api_auth_from(auth: &RegistryAuth) -> LofsResult<ApiAuth> {
    match auth {
        RegistryAuth::Basic(_, token) => Ok(ApiAuth::PrivateToken(token.clone())),
        RegistryAuth::Bearer(token) => Ok(ApiAuth::Bearer(token.clone())),
        RegistryAuth::Anonymous => Err(LofsError::Registry(
            "GitLab DELETE requires a Personal Access Token — supply --token (and optionally --username)".into(),
        )),
    }
}

/// Build a minimal `reqwest::Response` carrying the given status so the
/// trait's return type matches the OCI native-DELETE flow. We do this by
/// calling a dummy endpoint on the GitLab API that's guaranteed to
/// return the right status — but that's a round-trip too many, so we
/// avoid it by encoding the status through an inline channel.
///
/// `reqwest::Response::from(http::Response<Body>)` accepts a hand-built
/// response. We make it here.
fn synth_response(status: reqwest::StatusCode) -> LofsResult<reqwest::Response> {
    let inner = http::Response::builder()
        .status(status)
        .body(bytes::Bytes::from_static(b""))
        .map_err(|e| LofsError::Registry(format!("synth Response: {e}")))?;
    Ok(reqwest::Response::from(inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitlab_driver_flags() {
        let d = GitLabDriver::new();
        assert_eq!(d.name(), "gitlab");
        assert!(!d.supports_artifact_type());
        assert!(!d.supports_native_delete());
        assert_eq!(d.effective_repo_mode(""), RepoMode::Shared);
        assert_eq!(d.effective_repo_mode("user/project"), RepoMode::Shared);
    }

    #[test]
    fn rate_limit_is_bounded() {
        let p = GitLabDriver::new().rate_limit_policy();
        assert_eq!(p.max_concurrent, Some(10));
        assert!(p.max_retries >= 3);
    }

    #[test]
    fn api_auth_mapping() {
        let basic = RegistryAuth::Basic("u".into(), "tok".into());
        let mapped = api_auth_from(&basic).unwrap();
        matches!(mapped, ApiAuth::PrivateToken(_));

        let bearer = RegistryAuth::Bearer("tok".into());
        let mapped = api_auth_from(&bearer).unwrap();
        matches!(mapped, ApiAuth::Bearer(_));

        let anon = RegistryAuth::Anonymous;
        assert!(api_auth_from(&anon).is_err());
    }

    #[test]
    fn synth_response_carries_status() {
        let r = synth_response(reqwest::StatusCode::NO_CONTENT).unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::NO_CONTENT);
    }
}
