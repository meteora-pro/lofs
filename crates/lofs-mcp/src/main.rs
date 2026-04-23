//! `lofs-mcp` — MCP server exposing the LOFS four-tool surface
//! (`lofs.create / list / mount / unmount`).
//!
//! Concept scaffold only. The real server lands in Phase 1.1 / 1.2 —
//! see [ADR-001](../../../docs/architecture/adr/ADR-001-lofs.md),
//! [ADR-002](../../../docs/architecture/adr/ADR-002-cooperative-coordination.md),
//! and [IMPLEMENTATION_PLAN.md](../../../docs/IMPLEMENTATION_PLAN.md).

#![warn(clippy::all)]

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(
        core_version = lofs_core::VERSION,
        "lofs-mcp scaffold — MCP server not wired yet (Phase 2)"
    );
    eprintln!(
        "lofs-mcp is a concept scaffold. See docs/architecture/adr/ADR-001-lofs.md \
         for the planned L0 design. Not usable yet."
    );
}
