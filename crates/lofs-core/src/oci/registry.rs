//! Thin wrapper around `oci-client` specialised for LOFS bucket operations.
//!
//! Responsibilities:
//! - Translate our `Bucket` domain type into (config blob, manifest) pair
//!   and push it under a stable reference.
//! - Pull a bucket back by name (+ org) and reconstruct the domain object,
//!   preferring manifest annotations (cheap) with a config-blob fallback
//!   (full fidelity).
//! - Enumerate buckets via whatever mechanism the registry offers.
//! - Delete a bucket — `oci-client` doesn't expose manifest DELETE, so the
//!   DELETE round-trip lives here directly.
//!
//! ## Base URL grammar
//!
//! `base_url` may include a path prefix. Registries that host many
//! projects under a single hostname (GitLab Container Registry, Harbor
//! with `projects`, GHCR with orgs) need the prefix to scope everything
//! the CLI pushes/pulls under a specific project.
//!
//! ```text
//!   http://localhost:5100                 host=localhost:5100    prefix=""
//!   https://registry.gitlab.com           host=registry.gitlab.com  prefix=""
//!   https://registry.gitlab.com/me/repo   host=registry.gitlab.com  prefix="me/repo"
//! ```
//!
//! ## Repo mode (auto-selected from `base_url`)
//!
//! - `RepoMode::Separate` — used when there's no path prefix. Every
//!   bucket lives in its own repository:
//!   `registry/lofs/<org>/<name>:latest`. `lofs.list` walks
//!   `/v2/_catalog`. Good fit for a dedicated LOFS registry (local Zot,
//!   CNCF Distribution).
//!
//! - `RepoMode::Shared` — used when a path prefix is supplied. All
//!   buckets share a single repository and are distinguished by tag:
//!   `registry/<prefix>:<org>.<name>`. `lofs.list` calls `list_tags`
//!   on that one repo. Good fit for GitLab Container Registry, Harbor,
//!   and similar hosts that scope auth tokens to the project repo only
//!   (they refuse JWT access to `<prefix>/something/else`).
//!
//! The mode is selected automatically from the supplied URL; callers
//! almost never need to think about it.

use std::collections::BTreeMap;

use oci_client::client::{Client, ClientConfig};
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::secrets::RegistryAuth;
use oci_client::{Reference, RegistryOperation};
use reqwest::StatusCode;
use serde::Deserialize;

use super::manifest::{
    BucketConfig, ListedManifest, bucket_annotations, bucket_from_annotations, build_pair,
};
use super::media_types::{ANNOTATION_NS, BUCKET_CONFIG_V1};
use crate::bucket::{Bucket, BucketName};
use crate::error::{LofsError, LofsResult};

/// Top-level path segment under which all LOFS buckets live in the registry
/// when `Separate` mode is in use. Stable — rename would be a wire-compat break.
pub const NAMESPACE: &str = "lofs";

/// Repository path segment used in place of an org when the bucket is
/// "personal" (no org). Reserved — `NewBucket::try_new` rejects an explicit
/// `org="personal"` so user-supplied orgs never collide with the path we
/// synthesise for None-org buckets.
pub const PERSONAL_ORG_SEGMENT: &str = "personal";

/// Tag used for the HEAD bucket manifest in `Separate` mode. Historical
/// snapshot tags (`snap-<ts>`) are introduced in Phase 1.2; L0 only touches
/// this tag.
pub const HEAD_TAG: &str = "latest";

/// Repo-addressing model — how a bucket maps onto an OCI registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoMode {
    /// One OCI repository per bucket. Repo is `lofs/<org>/<name>`,
    /// tag is always `latest`. Chosen when the base URL has no path
    /// prefix (dedicated LOFS registry — Zot, Distribution).
    Separate,
    /// All buckets share a single OCI repository (the path prefix
    /// from `base_url`); each bucket is a tag on that repo. Chosen
    /// when a path prefix is supplied — GitLab, Harbor, GHCR-style
    /// project-scoped hosts refuse JWT access to sub-paths.
    Shared,
}

/// LOFS client on top of an OCI registry.
#[derive(Clone)]
pub struct OciRegistry {
    client: Client,
    http: reqwest::Client,
    /// Scheme + host + port only — `http://localhost:5100`. Everything
    /// below `/v2` on this origin is ours; the path prefix (if any) lives
    /// in `path_prefix` and is folded into repo names.
    origin: String,
    /// Host part without scheme — e.g. `registry.gitlab.com`. This is what
    /// `oci_client::Reference` calls "registry".
    registry_host: String,
    /// Optional path prefix, no leading/trailing slashes, empty when the
    /// caller passed just a bare host URL.
    path_prefix: String,
    mode: RepoMode,
    auth: RegistryAuth,
}

impl std::fmt::Debug for OciRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OciRegistry")
            .field("origin", &self.origin)
            .field("registry_host", &self.registry_host)
            .field("path_prefix", &self.path_prefix)
            .field("mode", &self.mode)
            .field("auth", &auth_shape(&self.auth))
            .finish()
    }
}

impl OciRegistry {
    /// Build an anonymous-auth client pointing at `base_url`.
    pub fn anonymous(base_url: impl AsRef<str>) -> LofsResult<Self> {
        let parsed = ParsedUrl::parse(base_url.as_ref())?;
        let protocol = if parsed.https {
            oci_client::client::ClientProtocol::Https
        } else {
            oci_client::client::ClientProtocol::Http
        };
        let client = Client::new(ClientConfig {
            protocol,
            ..Default::default()
        });
        let mode = if parsed.path_prefix.is_empty() {
            RepoMode::Separate
        } else {
            RepoMode::Shared
        };
        Ok(Self {
            client,
            http: reqwest::Client::new(),
            origin: parsed.origin,
            registry_host: parsed.host,
            path_prefix: parsed.path_prefix,
            mode,
            auth: RegistryAuth::Anonymous,
        })
    }

    /// Attach a bearer token for authenticated operations.
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = RegistryAuth::Bearer(token.into());
        self
    }

    /// Attach HTTP Basic credentials. For GitLab this is `(username, PAT)`
    /// with the PAT scoped to `read_registry` (+ `write_registry` for
    /// create/delete). Classic and fine-grained tokens both work; for
    /// fine-grained tokens the `Shared` repo mode is mandatory (enforced
    /// automatically when the base URL has a path prefix).
    pub fn with_basic(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.auth = RegistryAuth::Basic(username.into(), password.into());
        self
    }

    /// Origin URL (scheme + host + port). Stable for display / debug.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Registry host without scheme — the form `oci_client::Reference` wants.
    pub fn registry_host(&self) -> &str {
        &self.registry_host
    }

    /// Path prefix (may be empty).
    pub fn path_prefix(&self) -> &str {
        &self.path_prefix
    }

    /// Repo-addressing mode selected from the base URL.
    pub fn mode(&self) -> RepoMode {
        self.mode
    }

    /// Short identifier describing which auth mode is active — `"anonymous"`,
    /// `"bearer"`, or `"basic:<username>"`. Useful in `lofs doctor`.
    pub fn auth_label(&self) -> String {
        auth_shape(&self.auth)
    }

    /// Ping the registry's `/v2/` endpoint.
    pub async fn ping(&self) -> LofsResult<()> {
        let url = format!("{}/v2/", self.origin);
        let res = self.http.get(&url).send().await?;
        let code = res.status();
        if code.is_success() || code == StatusCode::UNAUTHORIZED {
            Ok(())
        } else {
            Err(LofsError::Registry(format!(
                "registry ping {}: HTTP {}",
                self.origin, code
            )))
        }
    }

    /// Push a bucket identity manifest (config blob + tag) to the registry.
    /// Returns the resulting manifest URL.
    pub async fn push_bucket(&self, bucket: &Bucket) -> LofsResult<String> {
        let (config, manifest) = build_pair(bucket)?;
        let reference = self.reference_for(bucket)?;
        let resp = self
            .client
            .push(&reference, &[], config, &self.auth, Some(manifest))
            .await?;
        Ok(resp.manifest_url)
    }

    /// Pull a bucket by name + org.
    pub async fn pull_bucket(&self, name: &BucketName, org: Option<&str>) -> LofsResult<Bucket> {
        let reference = self.reference_for_components(name.as_str(), org)?;
        let (manifest, _raw_json) = self
            .client
            .pull_image_manifest(&reference, &self.auth)
            .await
            .map_err(|e| {
                if is_manifest_not_found(&e) {
                    LofsError::NotFound(display_bucket(name, org))
                } else {
                    LofsError::Registry(e.to_string())
                }
            })?;

        let annotations = manifest.annotations.unwrap_or_default();
        if annotations.contains_key(&format!("{ANNOTATION_NS}.bucket_id"))
            && let Ok(b) = bucket_from_annotations(&annotations)
        {
            return Ok(b);
        }

        self.pull_bucket_from_config_blocking(&reference, &manifest.config.digest)
            .await
    }

    /// Enumerate bucket manifests across the registry, scoped to our
    /// repo-mode.
    pub async fn list_buckets(&self) -> LofsResult<Vec<Bucket>> {
        let listings = self.list_bucket_manifests().await?;
        let mut buckets = Vec::new();
        for listed in listings {
            if let Ok(b) = bucket_from_annotations(&listed.annotations) {
                buckets.push(b);
            }
        }
        Ok(buckets)
    }

    /// Enumerate the raw bucket manifest descriptors.
    pub async fn list_bucket_manifests(&self) -> LofsResult<Vec<ListedManifest>> {
        match self.mode {
            RepoMode::Separate => self.list_manifests_separate().await,
            RepoMode::Shared => self.list_manifests_shared().await,
        }
    }

    /// Delete a bucket: fetch the manifest digest, then issue DELETE.
    pub async fn delete_bucket(&self, name: &BucketName, org: Option<&str>) -> LofsResult<()> {
        let reference = self.reference_for_components(name.as_str(), org)?;
        let digest = self
            .client
            .fetch_manifest_digest(&reference, &self.auth)
            .await
            .map_err(|e| {
                if is_manifest_not_found(&e) {
                    LofsError::NotFound(display_bucket(name, org))
                } else {
                    LofsError::Registry(e.to_string())
                }
            })?;

        let url = format!(
            "{}/v2/{}/manifests/{}",
            self.origin,
            reference.repository(),
            digest
        );
        let res = self.authorised_delete(&url, &reference).await?;
        match res.status() {
            StatusCode::ACCEPTED | StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => Err(LofsError::NotFound(display_bucket(name, org))),
            StatusCode::METHOD_NOT_ALLOWED => Err(LofsError::Registry(
                "registry refused DELETE — ensure delete is enabled \
                 (REGISTRY_STORAGE_DELETE_ENABLED=true for Distribution; \
                 in GitLab CR deletion requires owner/maintainer permission \
                 + the `write_registry` scope)"
                    .into(),
            )),
            code => Err(LofsError::Registry(format!(
                "unexpected DELETE status for {}: {code}",
                display_bucket(name, org)
            ))),
        }
    }

    // --- internals ------------------------------------------------------

    fn reference_for(&self, bucket: &Bucket) -> LofsResult<Reference> {
        self.reference_for_components(bucket.name.as_str(), bucket.org.as_deref())
    }

    fn reference_for_components(&self, name: &str, org: Option<&str>) -> LofsResult<Reference> {
        let org_seg = match org {
            Some(o) if o == PERSONAL_ORG_SEGMENT => {
                return Err(LofsError::InvalidName {
                    name: o.to_string(),
                    reason: format!(
                        "org `{PERSONAL_ORG_SEGMENT}` is reserved for personal-scope buckets"
                    ),
                });
            }
            Some(o) if o.is_empty() => {
                return Err(LofsError::InvalidName {
                    name: o.to_string(),
                    reason: "org must not be empty".into(),
                });
            }
            Some(o) => o,
            None => PERSONAL_ORG_SEGMENT,
        };

        let (repo, tag) = match self.mode {
            RepoMode::Separate => {
                let repo = format!("{NAMESPACE}/{org_seg}/{name}");
                (repo, HEAD_TAG.to_string())
            }
            RepoMode::Shared => {
                let repo = if self.path_prefix.is_empty() {
                    NAMESPACE.to_string()
                } else {
                    self.path_prefix.clone()
                };
                let tag = encode_shared_tag(org_seg, name)?;
                (repo, tag)
            }
        };
        Ok(Reference::with_tag(self.registry_host.clone(), repo, tag))
    }

    fn shared_repo(&self) -> String {
        if self.path_prefix.is_empty() {
            NAMESPACE.to_string()
        } else {
            self.path_prefix.clone()
        }
    }

    /// `true` if `repo` sits under our path prefix + LOFS namespace.
    /// Only used for `Separate` mode catalog filtering.
    fn is_lofs_repo(&self, repo: &str) -> bool {
        let lofs_root = if self.path_prefix.is_empty() {
            NAMESPACE.to_string()
        } else {
            format!("{}/{NAMESPACE}", self.path_prefix)
        };
        repo == lofs_root || repo.starts_with(&format!("{lofs_root}/"))
    }

    async fn list_manifests_separate(&self) -> LofsResult<Vec<ListedManifest>> {
        let repos = self.fetch_catalog().await?;
        let mut out = Vec::new();
        for repo in repos {
            if !self.is_lofs_repo(&repo) {
                continue;
            }
            match self.load_manifest(&repo, HEAD_TAG).await {
                Ok(listed) if listed.is_bucket() => out.push(listed),
                Ok(_) => {}
                Err(LofsError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn list_manifests_shared(&self) -> LofsResult<Vec<ListedManifest>> {
        let repo = self.shared_repo();
        let reference = Reference::with_tag(
            self.registry_host.clone(),
            repo.clone(),
            // Any valid tag — `list_tags` doesn't actually consume it.
            HEAD_TAG.to_string(),
        );
        // `list_tags` deserialises `{ "tags": null }` (empty repo) as a
        // parse error — catch it and return an empty tag list instead.
        let tags_resp = match self
            .client
            .list_tags(&reference, &self.auth, None, None)
            .await
        {
            Ok(r) => r,
            Err(e) if is_manifest_not_found(&e) => return Ok(Vec::new()),
            Err(OciDistributionError::GenericError(msg)) => {
                return Err(LofsError::Registry(format!("list_tags {repo}: {msg:?}")));
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("invalid type: null") || msg.contains("null, expected") {
                    return Ok(Vec::new());
                }
                return Err(LofsError::Registry(format!("list_tags {repo}: {e}")));
            }
        };
        let mut out = Vec::new();
        for tag in tags_resp.tags {
            if !looks_like_bucket_tag(&tag) {
                continue;
            }
            match self.load_manifest(&repo, &tag).await {
                Ok(listed) if listed.is_bucket() => out.push(listed),
                Ok(_) => {}
                Err(LofsError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn pull_bucket_from_config_blocking(
        &self,
        reference: &Reference,
        digest: &str,
    ) -> LofsResult<Bucket> {
        let mut buf: Vec<u8> = Vec::new();
        self.client
            .pull_blob(reference, digest, &mut buf)
            .await
            .map_err(|e| LofsError::Registry(format!("pull config blob: {e}")))?;
        let cfg = BucketConfig::from_bytes(&buf)?;
        Ok(cfg.bucket)
    }

    async fn fetch_catalog(&self) -> LofsResult<Vec<String>> {
        let mut out = Vec::new();
        let mut next = format!("{}/v2/_catalog?n=200", self.origin);
        loop {
            let res = self.authorised_get(&next).await?;
            let status = res.status();
            if !status.is_success() {
                return Err(LofsError::Registry(format!(
                    "catalog GET {next}: HTTP {status}"
                )));
            }
            let link_header = res
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let body: CatalogResponse = res.json().await?;
            out.extend(body.repositories);
            match link_header.and_then(|h| parse_link_next(&h, &self.origin)) {
                Some(n) => next = n,
                None => break,
            }
        }
        Ok(out)
    }

    async fn load_manifest(&self, repo: &str, tag: &str) -> LofsResult<ListedManifest> {
        let reference = Reference::with_tag(
            self.registry_host.clone(),
            repo.to_string(),
            tag.to_string(),
        );
        let (manifest, digest) = self
            .client
            .pull_image_manifest(&reference, &self.auth)
            .await
            .map_err(|e| {
                if is_manifest_not_found(&e) {
                    LofsError::NotFound(format!("{repo}:{tag}"))
                } else {
                    LofsError::Registry(e.to_string())
                }
            })?;

        let annotations: BTreeMap<String, String> = manifest.annotations.unwrap_or_default();

        Ok(ListedManifest {
            tag_ref: format!("{repo}:{tag}"),
            digest,
            media_type: manifest
                .media_type
                .unwrap_or_else(|| BUCKET_CONFIG_V1.to_string()),
            annotations,
        })
    }

    async fn authorised_get(&self, url: &str) -> LofsResult<reqwest::Response> {
        let req = self.http.get(url);
        let req = apply_auth(req, &self.auth);
        Ok(req.send().await?)
    }

    /// DELETE with OCI token-exchange awareness: if the registry replies
    /// 401 + `WWW-Authenticate: Bearer realm=…`, exchange credentials for a
    /// JWT via `oci-client::auth()` and retry. Local Zot / Distribution
    /// accept Basic directly and never hit the retry branch; GitLab CR,
    /// Harbor and other project-scoped registries always do.
    async fn authorised_delete(
        &self,
        url: &str,
        reference: &Reference,
    ) -> LofsResult<reqwest::Response> {
        let first = apply_auth(self.http.delete(url), &self.auth).send().await?;
        if first.status() != StatusCode::UNAUTHORIZED {
            return Ok(first);
        }

        // Token-exchange fallback. `client.auth()` performs the Bearer
        // challenge → JWT-fetch round-trip with our cached `RegistryAuth`;
        // the returned token is scoped to the push op which GitLab grants
        // `delete` under (write_registry covers both push and delete).
        let maybe_jwt = self
            .client
            .auth(reference, &self.auth, RegistryOperation::Push)
            .await
            .map_err(|e| LofsError::Registry(format!("token exchange for DELETE: {e}")))?;

        match maybe_jwt {
            Some(jwt) => Ok(self.http.delete(url).bearer_auth(jwt).send().await?),
            None => Ok(first),
        }
    }
}

/// Encode a (org, name) pair into a single OCI tag for `Shared` mode.
///
/// Tag regex (per OCI distribution spec) is
/// `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`. We use `.` as the separator
/// between org and name — it's a tag character but **not** valid in
/// our `BucketName` ([a-z0-9-_]), so decoding is unambiguous.
///
/// Personal-scope buckets encode as just `<name>`; org-scoped as
/// `<org>.<name>`.
fn encode_shared_tag(org_seg: &str, name: &str) -> LofsResult<String> {
    let tag = if org_seg == PERSONAL_ORG_SEGMENT {
        name.to_string()
    } else {
        format!("{org_seg}.{name}")
    };
    if tag.len() > 128 {
        return Err(LofsError::InvalidName {
            name: tag,
            reason: "encoded tag exceeds 128 chars (reduce org/name length)".into(),
        });
    }
    Ok(tag)
}

/// Quick heuristic: does this tag look like a LOFS bucket tag?
///
/// We can't be 100% certain without pulling the manifest (which happens
/// next in `list_manifests_shared` anyway), but a conservative filter
/// avoids pulling manifests for obviously-unrelated tags sitting in the
/// same repo (e.g. actual container images). A LOFS tag is one of:
///
/// - `<name>` — personal bucket (no dot)
/// - `<org>.<name>` — org-scoped bucket (exactly one dot)
///
/// Anything that isn't shaped like a valid `BucketName` pair is skipped.
fn looks_like_bucket_tag(tag: &str) -> bool {
    match tag.split_once('.') {
        None => BucketName::new(tag.to_string()).is_ok(),
        Some((org, name)) if !name.contains('.') => {
            BucketName::new(org.to_string()).is_ok() && BucketName::new(name.to_string()).is_ok()
        }
        _ => false, // multi-dot → not us
    }
}

struct ParsedUrl {
    origin: String,
    host: String,
    path_prefix: String,
    https: bool,
}

impl ParsedUrl {
    fn parse(base_url: &str) -> LofsResult<Self> {
        let (rest, https) = if let Some(r) = base_url.strip_prefix("https://") {
            (r, true)
        } else if let Some(r) = base_url.strip_prefix("http://") {
            (r, false)
        } else {
            return Err(LofsError::Registry(format!(
                "base_url `{base_url}` must start with http:// or https://"
            )));
        };

        let trimmed = rest.trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(LofsError::Registry(format!(
                "base_url `{base_url}` has empty host"
            )));
        }

        let (host, path_prefix) = match trimmed.split_once('/') {
            Some((h, p)) => (h.to_string(), p.trim_matches('/').to_string()),
            None => (trimmed.to_string(), String::new()),
        };

        let scheme = if https { "https" } else { "http" };
        let origin = format!("{scheme}://{host}");

        Ok(Self {
            origin,
            host,
            path_prefix,
            https,
        })
    }
}

fn is_manifest_not_found(err: &OciDistributionError) -> bool {
    match err {
        OciDistributionError::ImageManifestNotFoundError(_) => true,
        OciDistributionError::RegistryError { envelope, .. } => envelope.errors.iter().any(|e| {
            matches!(
                e.code,
                OciErrorCode::ManifestUnknown | OciErrorCode::NameUnknown
            )
        }),
        _ => false,
    }
}

fn display_bucket(name: &BucketName, org: Option<&str>) -> String {
    match org {
        Some(o) => format!("{o}/{name}"),
        None => name.to_string(),
    }
}

fn parse_link_next(link_header: &str, origin: &str) -> Option<String> {
    for part in link_header.split(',') {
        let part = part.trim();
        if !part.contains("rel=\"next\"") {
            continue;
        }
        let start = part.find('<')?;
        let end = part.find('>')?;
        if end <= start + 1 {
            return None;
        }
        let path = &part[start + 1..end];
        return Some(format!("{}{path}", origin.trim_end_matches('/')));
    }
    None
}

fn apply_auth(req: reqwest::RequestBuilder, auth: &RegistryAuth) -> reqwest::RequestBuilder {
    match auth {
        RegistryAuth::Anonymous => req,
        RegistryAuth::Basic(user, pass) => req.basic_auth(user, Some(pass)),
        RegistryAuth::Bearer(token) => req.bearer_auth(token),
    }
}

fn auth_shape(auth: &RegistryAuth) -> String {
    match auth {
        RegistryAuth::Anonymous => "anonymous".into(),
        RegistryAuth::Basic(user, _) => format!("basic:{user}"),
        RegistryAuth::Bearer(_) => "bearer".into(),
    }
}

#[derive(Debug, Deserialize)]
struct CatalogResponse {
    #[serde(default)]
    repositories: Vec<String>,
}

pub use super::manifest::bucket_annotations as bucket_annotations_fn;

/// Thin façade for callers that just need the canonical map.
pub fn annotations_for(bucket: &Bucket) -> BTreeMap<String, String> {
    bucket_annotations(bucket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_host_http() {
        let p = ParsedUrl::parse("http://localhost:5100").unwrap();
        assert_eq!(p.host, "localhost:5100");
        assert_eq!(p.origin, "http://localhost:5100");
        assert_eq!(p.path_prefix, "");
        assert!(!p.https);
    }

    #[test]
    fn parse_with_path_prefix() {
        let p = ParsedUrl::parse("https://registry.gitlab.com/andreymaznyak/lofs-testbed").unwrap();
        assert_eq!(p.host, "registry.gitlab.com");
        assert_eq!(p.origin, "https://registry.gitlab.com");
        assert_eq!(p.path_prefix, "andreymaznyak/lofs-testbed");
    }

    #[test]
    fn parse_rejects_bare_host() {
        assert!(ParsedUrl::parse("localhost:5100").is_err());
    }

    #[test]
    fn mode_auto_detects_from_url() {
        let reg = OciRegistry::anonymous("http://localhost:5100").unwrap();
        assert_eq!(reg.mode(), RepoMode::Separate);

        let reg = OciRegistry::anonymous("https://registry.gitlab.com/me/repo").unwrap();
        assert_eq!(reg.mode(), RepoMode::Shared);
    }

    #[test]
    fn separate_reference_builds_subpath() {
        let reg = OciRegistry::anonymous("http://localhost:5100").unwrap();
        let r = reg
            .reference_for_components("foo", Some("meteora"))
            .unwrap();
        assert_eq!(r.repository(), "lofs/meteora/foo");
        assert_eq!(r.tag(), Some(HEAD_TAG));

        let r = reg.reference_for_components("bar", None).unwrap();
        assert_eq!(r.repository(), "lofs/personal/bar");
    }

    #[test]
    fn shared_reference_folds_identity_into_tag() {
        let reg = OciRegistry::anonymous("https://registry.gitlab.com/andreymaznyak/lofs-testbed")
            .unwrap();

        let r = reg
            .reference_for_components("teleport-demo", Some("meteora"))
            .unwrap();
        assert_eq!(r.repository(), "andreymaznyak/lofs-testbed");
        assert_eq!(r.tag(), Some("meteora.teleport-demo"));

        let r = reg.reference_for_components("teleport-demo", None).unwrap();
        assert_eq!(r.repository(), "andreymaznyak/lofs-testbed");
        assert_eq!(r.tag(), Some("teleport-demo"));
    }

    #[test]
    fn encode_shared_tag_enforces_length() {
        let long = "a".repeat(60);
        let tag = encode_shared_tag("meteora", &long).unwrap();
        assert!(tag.len() <= 128);

        // `meteora.` prefix (8 chars) + 121-char name = 129 chars, just over the limit.
        let too_long_name = "a".repeat(121);
        let err = encode_shared_tag("meteora", &too_long_name).unwrap_err();
        assert!(matches!(err, LofsError::InvalidName { .. }));
    }

    #[test]
    fn looks_like_bucket_tag_matches_encodings() {
        assert!(looks_like_bucket_tag("demo"));
        assert!(looks_like_bucket_tag("meteora.teleport-demo"));
        assert!(!looks_like_bucket_tag("demo.too.many.dots"));
        assert!(!looks_like_bucket_tag("UPPERCASE-rejected"));
        assert!(!looks_like_bucket_tag(""));
    }

    #[test]
    fn reserved_personal_org_is_rejected() {
        let reg = OciRegistry::anonymous("http://localhost:5100").unwrap();
        let err = reg
            .reference_for_components("foo", Some(PERSONAL_ORG_SEGMENT))
            .unwrap_err();
        assert!(matches!(err, LofsError::InvalidName { .. }));
    }

    #[test]
    fn is_lofs_repo_bare_host() {
        let reg = OciRegistry::anonymous("http://localhost:5100").unwrap();
        assert!(reg.is_lofs_repo("lofs"));
        assert!(reg.is_lofs_repo("lofs/meteora/foo"));
        assert!(!reg.is_lofs_repo("other/foo"));
    }

    #[test]
    fn parse_link_next_extracts_path() {
        let h = "</v2/_catalog?n=200&last=lofs/a>; rel=\"next\"";
        let got = parse_link_next(h, "http://localhost:5100").unwrap();
        assert_eq!(got, "http://localhost:5100/v2/_catalog?n=200&last=lofs/a");
    }

    #[test]
    fn auth_shape_roundtrip() {
        let reg = OciRegistry::anonymous("http://localhost:5100").unwrap();
        assert_eq!(reg.auth_label(), "anonymous");
        assert_eq!(reg.clone().with_bearer("tok").auth_label(), "bearer");
        assert_eq!(reg.with_basic("alice", "pw").auth_label(), "basic:alice");
    }
}
