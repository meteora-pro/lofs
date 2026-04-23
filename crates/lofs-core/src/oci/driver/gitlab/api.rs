//! Thin HTTP wrapper for GitLab's REST API.
//!
//! We only need two endpoints for the LOFS driver:
//!
//! - `GET /api/v4/projects/:urlencoded_path` → the project's numeric id.
//! - `GET /api/v4/projects/:id/registry/repositories` → list repositories
//!   so we can match the one whose `path` equals our project prefix.
//! - `DELETE /api/v4/projects/:id/registry/repositories/:rid/tags/:tag` →
//!   remove a bucket tag (`<org>.<name>` or bare `<name>`).
//!
//! Everything is behind `reqwest` with the same `rustls`-backed TLS stack
//! the rest of the crate uses.

use reqwest::header::HeaderValue;
use serde::Deserialize;

use crate::error::{LofsError, LofsResult};

/// Stateless GitLab API client. Constructed per-operation — no persistent
/// connection pool state beyond whatever `reqwest::Client` reuses across
/// the process.
#[derive(Debug, Clone)]
pub struct GitLabApi {
    http: reqwest::Client,
    /// Base URL of the GitLab instance API root — e.g.
    /// `https://gitlab.com/api/v4`. NOT the registry host.
    api_base: String,
    auth: ApiAuth,
}

/// Auth method for API calls. GitLab accepts PATs via `PRIVATE-TOKEN`
/// header, deploy tokens via HTTP Basic, OAuth access tokens via
/// `Authorization: Bearer`. We cover the PAT path (most common); the
/// others are simple future adds.
#[derive(Debug, Clone)]
pub enum ApiAuth {
    /// Personal Access Token (classic or fine-grained) — sent via the
    /// `PRIVATE-TOKEN` header.
    PrivateToken(String),
    /// OAuth / deploy token — sent as `Authorization: Bearer <token>`.
    Bearer(String),
    /// No auth — `/projects/:id` of a public project still resolves.
    Anonymous,
}

impl GitLabApi {
    /// Build an API client talking to `api_base` (e.g. `https://gitlab.com/api/v4`).
    pub fn new(api_base: impl Into<String>, auth: ApiAuth) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_base: api_base.into(),
            auth,
        }
    }

    /// Convenience: derive the API base URL from the container-registry
    /// host. `registry.gitlab.com` → `https://gitlab.com`, and any
    /// `registry.<domain>` → `https://<domain>`. Falls back to the same
    /// host when nothing matches (self-hosted GitLab could register the
    /// registry on the same hostname).
    pub fn derive_from_registry_host(registry_host: &str) -> String {
        let cleaned = registry_host
            .strip_prefix("registry.")
            .unwrap_or(registry_host);
        format!("https://{cleaned}/api/v4")
    }

    /// Look up a project by its URL path (e.g. `andreymaznyak/lofs-testbed`).
    /// The path MUST be URL-encoded — GitLab insists on encoding the `/`.
    pub async fn get_project(&self, path: &str) -> LofsResult<ProjectInfo> {
        let url = format!(
            "{}/projects/{}",
            self.api_base,
            urlencoding_encode_slashes(path)
        );
        let req = self.auth_headers(self.http.get(&url));
        let resp = req.send().await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(LofsError::NotFound(format!("project `{path}`")));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LofsError::Registry(format!(
                "GitLab API GET {url} → HTTP {status}: {body}"
            )));
        }
        Ok(resp.json().await?)
    }

    /// List registry repositories for a project. Pagination-free — we
    /// grab the first page only, since LOFS's bucket repo is always the
    /// canonical one and shows up at the top (GitLab orders by id ASC by
    /// default which tends to place the project's "default" repo first).
    /// Extend to paginated if a user hits the edge case.
    pub async fn list_registry_repositories(
        &self,
        project_id: u64,
    ) -> LofsResult<Vec<RegistryRepository>> {
        let url = format!(
            "{}/projects/{project_id}/registry/repositories?per_page=100",
            self.api_base
        );
        let req = self.auth_headers(self.http.get(&url));
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LofsError::Registry(format!(
                "GitLab API GET {url} → HTTP {status}: {body}"
            )));
        }
        Ok(resp.json().await?)
    }

    /// Delete a single tag from a registry repository.
    /// `DELETE /projects/:id/registry/repositories/:rid/tags/:tag`.
    pub async fn delete_registry_tag(
        &self,
        project_id: u64,
        repo_id: u64,
        tag: &str,
    ) -> LofsResult<reqwest::StatusCode> {
        let url = format!(
            "{}/projects/{project_id}/registry/repositories/{repo_id}/tags/{tag}",
            self.api_base
        );
        let req = self.auth_headers(self.http.delete(&url));
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() && status != reqwest::StatusCode::NO_CONTENT {
            let body = resp.text().await.unwrap_or_default();
            return Err(LofsError::Registry(format!(
                "GitLab API DELETE {url} → HTTP {status}: {body}"
            )));
        }
        Ok(status)
    }

    fn auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            ApiAuth::Anonymous => req,
            ApiAuth::Bearer(t) => req.bearer_auth(t),
            ApiAuth::PrivateToken(t) => {
                // GitLab prefers `PRIVATE-TOKEN` specifically; it also
                // accepts it on the `Authorization` header for PATs, but
                // only when sent as `Bearer`, which would confuse proxies
                // that sniff for plain PATs.
                let value =
                    HeaderValue::from_str(t).unwrap_or_else(|_| HeaderValue::from_static(""));
                req.header("PRIVATE-TOKEN", value)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ProjectInfo {
    pub id: u64,
    #[serde(default)]
    pub path_with_namespace: String,
}

#[derive(Debug, Deserialize)]
pub struct RegistryRepository {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    /// Full path of the repository — e.g. `andreymaznyak/lofs-testbed` for
    /// the project-default repo, or `andreymaznyak/lofs-testbed/app` for
    /// a sub-path.
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub location: String,
}

/// Encode path segments while preserving `%2F` for slashes — which is
/// exactly the format GitLab's `/projects/:id` endpoint wants when `:id`
/// is the namespaced path. We avoid pulling in `urlencoding` for one call
/// site — the input alphabet is restricted enough to inline.
fn urlencoding_encode_slashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for b in s.as_bytes() {
        match *b {
            b'/' => out.push_str("%2F"),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_base_derived_from_registry_host() {
        assert_eq!(
            GitLabApi::derive_from_registry_host("registry.gitlab.com"),
            "https://gitlab.com/api/v4"
        );
        assert_eq!(
            GitLabApi::derive_from_registry_host("registry.gitlab.example.org"),
            "https://gitlab.example.org/api/v4"
        );
        assert_eq!(
            GitLabApi::derive_from_registry_host("gitlab.internal"),
            "https://gitlab.internal/api/v4",
            "no `registry.` prefix → same host"
        );
    }

    #[test]
    fn urlencoding_escapes_slashes_only() {
        assert_eq!(urlencoding_encode_slashes("user/project"), "user%2Fproject");
        assert_eq!(urlencoding_encode_slashes("a/b/c.d-e_f"), "a%2Fb%2Fc.d-e_f");
        assert_eq!(urlencoding_encode_slashes("hello"), "hello");
    }

    #[test]
    fn project_info_deserializes_minimal_json() {
        let json = r#"{"id":81569637,"path_with_namespace":"andreymaznyak/lofs-testbed"}"#;
        let p: ProjectInfo = serde_json::from_str(json).unwrap();
        assert_eq!(p.id, 81569637);
        assert_eq!(p.path_with_namespace, "andreymaznyak/lofs-testbed");
    }

    #[test]
    fn registry_repository_deserializes_minimal_json() {
        let json = r#"{"id":13,"name":"","path":"andreymaznyak/lofs-testbed","location":"registry.gitlab.com/andreymaznyak/lofs-testbed"}"#;
        let r: RegistryRepository = serde_json::from_str(json).unwrap();
        assert_eq!(r.id, 13);
        assert_eq!(r.path, "andreymaznyak/lofs-testbed");
    }
}
