//! `lofs-mcp` — MCP server exposing the LOFS four-tool surface.
//!
//! Concept scaffold only. The real server is planned for Phase 2 (see
//! [ADR-001](../../../docs/architecture/adr/ADR-001-lofs.md)).

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
