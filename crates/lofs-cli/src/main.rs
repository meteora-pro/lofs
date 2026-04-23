//! `lofs` — command-line interface for LOFS (Layered Overlay File System).
//!
//! L0 MVP: `doctor`, `create`, `list`, `stat`, `rm` against an OCI registry
//! ([ADR-002](../../../docs/architecture/adr/ADR-002-cooperative-coordination.md)).
//! `mount` / `unmount` / `status` are scaffolded — they land with the FUSE
//! + intent-manifest backend in Phase 1.2.
//!
//! See [IMPLEMENTATION_PLAN.md](../../../docs/IMPLEMENTATION_PLAN.md) for the
//! phased roadmap.

#![warn(clippy::all)]

use std::process::ExitCode;

use anyhow::Context as _;
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use lofs_core::bucket::{Bucket, BucketName, BucketStatus};
use lofs_core::error::LofsError;
use lofs_core::oci::{OciRegistry, driver_by_name_or_auto};
use lofs_core::{NewBucket, VERSION};
use serde::Serialize;

/// Default registry URL used by every subcommand. Matches the `zot` service
/// in `docker/docker-compose.yml`.
const DEFAULT_REGISTRY: &str = "http://localhost:5100";

#[derive(Parser, Debug)]
#[command(
    name = "lofs",
    version = VERSION,
    about = "Ephemeral shared workspace primitive for multi-agent AI systems",
    long_about = None,
)]
struct Cli {
    /// OCI registry base URL, optionally including a project path prefix.
    /// Examples:
    ///   http://localhost:5100                                 (local Zot)
    ///   https://registry.gitlab.com/<user>/<project>           (GitLab)
    ///   https://harbor.internal/library                        (Harbor project)
    #[arg(long, global = true, env = "LOFS_REGISTRY", default_value = DEFAULT_REGISTRY)]
    registry: String,

    /// Username for HTTP Basic auth. On GitLab this is your GitLab account
    /// username; on Harbor it's the robot/account name. Ignored unless
    /// `--token` is also set (otherwise the CLI stays anonymous).
    #[arg(long, global = true, env = "LOFS_REGISTRY_USERNAME")]
    username: Option<String>,

    /// Bearer or Basic password/token. If `--username` is also set, we send
    /// this as HTTP Basic (`<username>:<token>`); otherwise we send it as a
    /// bare Bearer token. For GitLab, generate a Personal Access Token with
    /// scopes `read_registry` (+ `write_registry` for create/rm).
    #[arg(
        long,
        global = true,
        env = "LOFS_REGISTRY_TOKEN",
        hide_env_values = true
    )]
    token: Option<String>,

    /// Registry flavour driver. `auto` (default) picks from the hostname:
    ///   registry.gitlab.com, *.gitlab.* → `gitlab`
    ///   ghcr.io, *.ghcr.io             → `ghcr` (scaffold)
    ///   everything else                 → `generic` (OCI 1.1 baseline —
    ///                                     Zot, Distribution, Harbor)
    /// Valid explicit values: generic | gitlab | ghcr | harbor | auto.
    /// Useful for self-hosted installs with non-obvious hostnames.
    #[arg(long, global = true, env = "LOFS_DRIVER", default_value = "auto")]
    driver: String,

    /// Increase log verbosity (`-v`, `-vv`, `-vvv`).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Allocate a new bucket.
    Create(CreateArgs),
    /// List buckets.
    List(ListArgs),
    /// Show detailed info for a single bucket.
    Stat(StatArgs),
    /// Permanently delete a bucket record.
    Rm(RmArgs),
    /// Environment / health check (platform, registry URL, ping).
    Doctor,

    /// Mount a bucket as a FUSE overlay. **Not implemented yet** (Phase 1.2).
    Mount(MountArgs),
    /// Unmount an active session. **Not implemented yet** (Phase 1.2).
    Unmount(UnmountArgs),
    /// List active mount sessions for this host. **Not implemented yet**.
    Status,
}

#[derive(clap::Args, Debug)]
struct CreateArgs {
    /// Bucket name (`[a-z0-9][a-z0-9-_]{1,62}`).
    name: String,
    /// Days until the bucket auto-expires.
    #[arg(long, default_value_t = 7)]
    ttl_days: i64,
    /// On-registry size cap in megabytes.
    #[arg(long)]
    size_limit_mb: Option<i64>,
    /// Logical owner (org / team / user). Omit for personal scope.
    #[arg(long)]
    org: Option<String>,
    /// Emit result as JSON instead of a text summary.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(clap::Args, Debug)]
struct ListArgs {
    /// Restrict to a single org (client-side filter).
    #[arg(long)]
    org: Option<String>,
    /// Substring match against bucket names (client-side filter).
    #[arg(long)]
    filter: Option<String>,
    /// Include expired buckets (by default only `active` are shown).
    #[arg(long, short = 'a')]
    all: bool,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    format: OutputFormat,
}

#[derive(clap::Args, Debug)]
struct StatArgs {
    /// Bucket name.
    name: String,
    /// Org scope when resolving by name.
    #[arg(long)]
    org: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(clap::Args, Debug)]
struct RmArgs {
    /// Bucket name.
    name: String,
    /// Org scope when resolving by name.
    #[arg(long)]
    org: Option<String>,
    /// Skip the "are you sure" prompt (no-op in MVP — prompt not wired yet).
    #[arg(long, short = 'f')]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct MountArgs {
    /// Bucket name.
    name: String,
    /// Mount mode.
    #[arg(long, value_enum, default_value_t = MountMode::Ro)]
    mode: MountMode,
    /// Purpose string shown to other agents in advisory payloads.
    #[arg(long)]
    purpose: Option<String>,
    /// Expected duration of the session in seconds.
    #[arg(long)]
    duration: Option<u64>,
    /// Org scope when resolving by name.
    #[arg(long)]
    org: Option<String>,
}

#[derive(clap::Args, Debug)]
struct UnmountArgs {
    /// Session id returned by `lofs mount`.
    session: String,
    /// Commit the overlay as a new snapshot.
    #[arg(long, conflicts_with = "discard")]
    commit: bool,
    /// Drop the overlay without pushing a new snapshot.
    #[arg(long)]
    discard: bool,
}

#[derive(ValueEnum, Copy, Clone, Debug, PartialEq, Eq)]
enum OutputFormat {
    /// Table / human-readable text (default for list/stat).
    Table,
    /// Human-readable single-item text (default for create/stat).
    Text,
    /// Newline-delimited JSON.
    Json,
}

#[derive(ValueEnum, Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum MountMode {
    Ro,
    Rw,
    Fork,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match tokio_runtime().block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn init_tracing(verbose: u8) {
    let default_level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!(
            "lofs={default_level},lofs_core={default_level},lofs_cli={default_level}"
        ))
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let creds = Creds {
        registry: cli.registry,
        username: cli.username,
        token: cli.token,
        driver: cli.driver,
    };
    match cli.cmd {
        Cmd::Doctor => cmd_doctor(creds).await,
        Cmd::Create(args) => cmd_create(creds, args).await,
        Cmd::List(args) => cmd_list(creds, args).await,
        Cmd::Stat(args) => cmd_stat(creds, args).await,
        Cmd::Rm(args) => cmd_rm(creds, args).await,
        Cmd::Mount(_) | Cmd::Unmount(_) | Cmd::Status => Err(unsupported_mount_error().into()),
    }
}

#[derive(Clone, Debug)]
struct Creds {
    registry: String,
    username: Option<String>,
    token: Option<String>,
    driver: String,
}

fn open_registry(creds: &Creds) -> anyhow::Result<OciRegistry> {
    // Parse just enough of the URL to resolve the driver by hostname,
    // then hand it to OciRegistry alongside the full URL.
    let host = host_from_url(&creds.registry).with_context(|| {
        format!(
            "parse registry URL `{}` for driver detection",
            creds.registry
        )
    })?;
    let driver = driver_by_name_or_auto(&creds.driver, &host)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("resolve --driver={}", creds.driver))?;

    let reg = OciRegistry::anonymous_with_driver(&creds.registry, driver)
        .with_context(|| format!("configure OCI registry at {}", creds.registry))?;

    let reg = match (creds.username.as_deref(), creds.token.as_deref()) {
        (Some(user), Some(token)) => reg.with_basic(user, token),
        (None, Some(token)) => reg.with_bearer(token),
        (Some(_), None) => {
            // Username without token would silently stay anonymous — louder
            // to refuse it than to mislead.
            return Err(anyhow::anyhow!(
                "--username supplied without --token; either drop the username \
                 or provide a password/token (also via LOFS_REGISTRY_TOKEN)"
            ));
        }
        (None, None) => reg,
    };
    Ok(reg)
}

fn host_from_url(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| anyhow::anyhow!("URL must start with http:// or https://"))?;
    let trimmed = rest.trim_end_matches('/');
    Ok(match trimmed.split_once('/') {
        Some((h, _)) => h.to_string(),
        None => trimmed.to_string(),
    })
}

async fn cmd_doctor(creds: Creds) -> anyhow::Result<()> {
    let platform = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    println!("lofs {VERSION}");
    println!("platform:   {platform}/{arch}");
    println!("registry:   {}", creds.registry);

    match open_registry(&creds) {
        Ok(reg) => {
            println!(
                "driver:     {} ({})",
                reg.driver().name(),
                reg.driver().description()
            );
            println!("mode:       {:?}", reg.mode());
            println!("auth:       {}", reg.auth_label());
            if !reg.path_prefix().is_empty() {
                println!("prefix:     {}", reg.path_prefix());
            }
            let caps = reg.driver();
            println!(
                "caps:       artifactType={}, nativeDelete={}, catalog={}",
                caps.supports_artifact_type(),
                caps.supports_native_delete(),
                caps.catalog_supported()
            );
            match reg.ping().await {
                Ok(()) => match reg.list_buckets().await {
                    Ok(buckets) => {
                        println!("status:     ok ({} bucket(s) visible)", buckets.len())
                    }
                    Err(e) => println!("status:     reachable, list failed — {e}"),
                },
                Err(e) => println!("status:     ERROR — {e}"),
            }
        }
        Err(e) => println!("status:     ERROR — {e:#}"),
    }

    println!("mount:      not supported on `{platform}` (MVP ships Linux backend in Phase 1.2)");
    Ok(())
}

async fn cmd_create(creds: Creds, args: CreateArgs) -> anyhow::Result<()> {
    let reg = open_registry(&creds)?;
    let nb = NewBucket::try_new(
        args.name.clone(),
        args.org.clone(),
        args.ttl_days,
        args.size_limit_mb,
    )?;
    let bucket = nb.into_bucket_at(Utc::now());
    reg.push_bucket(&bucket)
        .await
        .map_err(anyhow::Error::from)
        .with_context(|| format!("push bucket `{}` to registry", args.name))?;

    emit_single("bucket created", &bucket, args.format);
    Ok(())
}

async fn cmd_list(creds: Creds, args: ListArgs) -> anyhow::Result<()> {
    let reg = open_registry(&creds)?;
    let mut buckets = reg.list_buckets().await?;

    if let Some(org) = &args.org {
        buckets.retain(|b| b.org.as_deref() == Some(org.as_str()));
    }
    if let Some(sub) = &args.filter {
        let needle = sub.to_lowercase();
        buckets.retain(|b| b.name.as_str().to_lowercase().contains(&needle));
    }
    if !args.all {
        buckets.retain(|b| b.status == BucketStatus::Active);
    }

    // Registry listing order is undefined — sort newest-first for stable UX.
    buckets.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    match args.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&buckets)?;
            println!("{json}");
        }
        OutputFormat::Table | OutputFormat::Text => render_table(&buckets),
    }
    Ok(())
}

async fn cmd_stat(creds: Creds, args: StatArgs) -> anyhow::Result<()> {
    let reg = open_registry(&creds)?;
    let name = BucketName::new(args.name.clone())?;
    let bucket = reg
        .pull_bucket(&name, args.org.as_deref())
        .await
        .map_err(map_not_found(&args.name))?;
    emit_single("bucket", &bucket, args.format);
    Ok(())
}

async fn cmd_rm(creds: Creds, args: RmArgs) -> anyhow::Result<()> {
    let reg = open_registry(&creds)?;
    let name = BucketName::new(args.name.clone())?;

    if !args.force {
        tracing::warn!(
            "rm without --force: interactive confirmation is not wired in MVP; \
             proceeding"
        );
    }

    reg.delete_bucket(&name, args.org.as_deref())
        .await
        .map_err(map_not_found(&args.name))?;
    println!("deleted {}", args.name);
    Ok(())
}

fn map_not_found(target: &str) -> impl Fn(LofsError) -> LofsError + '_ {
    move |e| match e {
        LofsError::NotFound(_) => LofsError::NotFound(target.to_string()),
        other => other,
    }
}

fn unsupported_mount_error() -> LofsError {
    LofsError::UnsupportedPlatform(format!(
        "mount/unmount/status land in Phase 1.2 (intent-manifest backend + FUSE). \
         Current host: {}/{}.",
        std::env::consts::OS,
        std::env::consts::ARCH
    ))
}

fn emit_single(label: &str, bucket: &Bucket, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(bucket).expect("bucket always serialises");
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Table => {
            println!("{label}: {}", bucket.name);
            println!("  id           {}", bucket.id);
            println!("  org          {}", bucket.org.as_deref().unwrap_or("-"));
            println!("  status       {}", bucket.status);
            println!("  ttl_days     {}", bucket.ttl_days);
            println!("  size_limit   {} MB", bucket.size_limit_mb);
            println!("  created_at   {}", bucket.created_at.to_rfc3339());
            println!("  expires_at   {}", bucket.expires_at.to_rfc3339());
            let remaining = bucket.remaining_at(Utc::now());
            if remaining.num_seconds() > 0 {
                println!(
                    "  remaining    {} day(s), {} hour(s)",
                    remaining.num_days(),
                    remaining.num_hours() % 24
                );
            } else {
                println!("  remaining    expired");
            }
        }
    }
}

fn render_table(buckets: &[Bucket]) {
    if buckets.is_empty() {
        println!("(no buckets)");
        return;
    }

    let headers = [
        "NAME", "ORG", "STATUS", "TTL(d)", "SIZE(MB)", "EXPIRES", "ID",
    ];
    let rows: Vec<[String; 7]> = buckets
        .iter()
        .map(|b| {
            [
                b.name.to_string(),
                b.org.clone().unwrap_or_else(|| "-".into()),
                status_label(b.status, Utc::now() >= b.expires_at),
                b.ttl_days.to_string(),
                b.size_limit_mb.to_string(),
                b.expires_at.format("%Y-%m-%d %H:%M").to_string(),
                short_id(&b.id.to_string()),
            ]
        })
        .collect();

    let mut widths = [0usize; 7];
    for (i, h) in headers.iter().enumerate() {
        widths[i] = h.len();
    }
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    print_row(&headers, &widths);
    print_row(
        &widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        &widths,
    );
    for row in &rows {
        print_row(&row.iter().map(String::as_str).collect::<Vec<_>>(), &widths);
    }
}

fn print_row(cells: &[&str], widths: &[usize]) {
    let parts: Vec<String> = cells
        .iter()
        .zip(widths.iter())
        .map(|(cell, w)| format!("{cell:<w$}", w = *w))
        .collect();
    println!("{}", parts.join("  "));
}

fn status_label(status: BucketStatus, expired_in_memory: bool) -> String {
    match status {
        BucketStatus::Active if expired_in_memory => "active (ttl elapsed)".into(),
        s => s.to_string(),
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
