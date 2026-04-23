//! `GenericDriver` — baseline OCI Distribution 1.1 behaviour.
//!
//! Used for local Zot, CNCF Distribution, Harbor with DELETE enabled,
//! and anything else that implements the spec without further quirks.
//! All of `RegistryDriver`'s methods use their default here, so the type
//! is effectively a tag.

use async_trait::async_trait;
use oci_client::RegistryOperation;
use reqwest::StatusCode;

use super::{DeleteContext, RegistryDriver};
use crate::error::{LofsError, LofsResult};

/// Baseline OCI 1.1 driver. Uses every default on [`RegistryDriver`].
#[derive(Debug, Default, Clone, Copy)]
pub struct GenericDriver;

impl GenericDriver {
    /// Construct a fresh driver. Stateless.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RegistryDriver for GenericDriver {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn description(&self) -> &'static str {
        "baseline OCI Distribution 1.1 (Zot, CNCF Distribution, Harbor, …)"
    }
}

/// Perform an OCI-spec DELETE on the manifest, with a token-exchange
/// retry if the registry replies 401 + `WWW-Authenticate: Bearer`.
///
/// This is factored out of the trait so other drivers can delegate here
/// for their "when-possible" path and only take over with a native API
/// when strictly necessary.
pub(crate) async fn perform_native_delete(
    ctx: &DeleteContext<'_>,
) -> LofsResult<reqwest::Response> {
    let url = format!(
        "{}/v2/{}/manifests/{}",
        ctx.origin,
        ctx.reference.repository(),
        ctx.digest
    );

    // First attempt — Basic/Bearer from the caller's credentials. On
    // registries that are content with that (Zot, Distribution), we're
    // done here. `retry_on_429` handles any rate-limit pushback along
    // the way.
    let first = ctx
        .limiter
        .retry_on_429(|| apply_auth(ctx.http.delete(&url), ctx.auth).send())
        .await?;
    if first.status() != StatusCode::UNAUTHORIZED {
        return Ok(first);
    }

    // Token-exchange fallback. `client.auth()` performs the Bearer
    // challenge → JWT-fetch round-trip with our cached `RegistryAuth`.
    let maybe_jwt = ctx
        .oci
        .auth(ctx.reference, ctx.auth, RegistryOperation::Push)
        .await
        .map_err(|e| LofsError::Registry(format!("token exchange for DELETE: {e}")))?;

    match maybe_jwt {
        Some(jwt) => Ok(ctx
            .limiter
            .retry_on_429(|| ctx.http.delete(&url).bearer_auth(jwt.clone()).send())
            .await?),
        None => Ok(first),
    }
}

fn apply_auth(
    req: reqwest::RequestBuilder,
    auth: &oci_client::secrets::RegistryAuth,
) -> reqwest::RequestBuilder {
    use oci_client::secrets::RegistryAuth;
    match auth {
        RegistryAuth::Anonymous => req,
        RegistryAuth::Basic(user, pass) => req.basic_auth(user, Some(pass)),
        RegistryAuth::Bearer(token) => req.bearer_auth(token),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_driver_identifies_itself() {
        let d = GenericDriver::new();
        assert_eq!(d.name(), "generic");
        assert!(d.description().contains("OCI"));
    }
}
