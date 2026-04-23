//! `HarborDriver` — Harbor / VMware Container Registry placeholder.
//!
//! **Status:** scaffold. Harbor implements the OCI Distribution spec
//! closely on top of CNCF Distribution, so the [`GenericDriver`] already
//! works against it when the admin has enabled delete. This driver will
//! diverge from Generic once we add:
//!
//! - **Project/robot-account auth helpers.** Harbor exposes robot
//!   accounts scoped to one project; they authenticate via HTTP Basic
//!   but the password rotates via the admin UI. We'll add a token-refresh
//!   hook once real Harbor deployments start exercising the driver.
//! - **Rate-limit headers.** Harbor forwards upstream backend latency but
//!   doesn't hard-cap by default; large deployments front it with an
//!   nginx rate-limit. We'll add Retry-After parsing if needed.
//! - **Replication-aware paths.** Harbor's distributed replication can
//!   surface stale catalog entries during fan-out; eventually
//!   `catalog_supported=false` for replicated clusters might be the
//!   right call.
//!
//! Today: same behaviour as [`GenericDriver`], just labelled `harbor`.

use async_trait::async_trait;

use super::RegistryDriver;

/// Placeholder driver for Harbor. Inherits every default from
/// [`RegistryDriver`] for now.
#[derive(Debug, Default, Clone, Copy)]
pub struct HarborDriver;

impl HarborDriver {
    /// Build a fresh driver.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RegistryDriver for HarborDriver {
    fn name(&self) -> &'static str {
        "harbor"
    }

    fn description(&self) -> &'static str {
        "VMware Harbor (scaffold — baseline OCI 1.1 behaviour today)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harbor_identifies_itself() {
        let d = HarborDriver::new();
        assert_eq!(d.name(), "harbor");
    }

    #[test]
    fn harbor_uses_defaults() {
        let d = HarborDriver::new();
        assert!(d.supports_artifact_type());
        assert!(d.supports_native_delete());
        assert!(d.catalog_supported());
        assert!(d.rate_limit_policy().max_concurrent.is_none());
    }
}
