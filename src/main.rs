// Use mimalloc for better multi-core performance (especially important for musl builds)
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
mod config;
mod constants;
mod daemon;
mod manifest;
#[cfg(feature = "registry")]
mod registry;
mod reliability;
mod runtime;
mod utils;

const AFTER_HELP: &str = "\
COMMON WORKFLOWS:
  # Start a new project
  mik new my-service
  cd my-service
  mik build -rc
  mik run

  # Add dependencies (auto-downloads)
  mik add user/repo                 # OCI: ghcr.io/user/repo:latest
  mik add user/repo:v1.0.0          # OCI with tag
  mik add --path ../my-component    # Local development

  # Build and deploy
  mik build --release --compose
  mik publish --tag v1.0.0

EXAMPLES:
  mik new my-api --lib              Create library component
  mik add user/router:v2.0          Add versioned dependency
  mik build -rc                     Release build with composition
  mik run                           Run on port 3000

For more help, see: https://github.com/dufeut/mik";

#[derive(Parser)]
#[command(name = "mik")]
#[command(version)]
#[command(about = "mik CLI - Build portable WASI HTTP components")]
#[command(
    long_about = "Package manager and build tool for WASI HTTP components.\n\nSimilar to cargo, uv, or pnpm but for WebAssembly components."
)]
#[command(after_help = AFTER_HELP)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Show detailed version information including dependencies
    #[arg(long, global = true)]
    version_verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    // =========================================================================
    // Daemon Commands (Docker-like instance management)
    // =========================================================================
    /// Start the daemon for process management and scheduling
    ///
    /// Starts a background daemon for managing WASM instances, cron jobs,
    /// and service discovery. Exposes a REST API for remote management.
    ///
    /// Examples:
    ///   mik daemon                 # Start on default port 9999
    ///   mik daemon --port 8080     # Start on custom port
    Daemon {
        /// Port for the daemon API (default: 9999)
        #[arg(short, long, default_value = "9999")]
        port: u16,
    },
    /// Start a WASM instance in the background
    ///
    /// Spawns a new mik server as a background process.
    /// Instances are tracked and can be managed with ps, down, logs commands.
    ///
    /// Examples:
    ///   mik up                          # Start on port 3000 as "default"
    ///   mik up --port 3001 --name dev   # Start on custom port with name
    ///   mik up --watch                  # Start with hot reload enabled
    Up {
        /// Instance name (default: "default")
        #[arg(short, long, default_value = "default")]
        name: String,
        /// Port for the HTTP server (default: 3000)
        #[arg(short, long, default_value = "3000")]
        port: u16,
        /// Watch for module changes and hot reload
        #[arg(short, long)]
        watch: bool,
    },
    /// Stop a running WASM instance
    ///
    /// Gracefully stops the instance, waiting for in-flight requests.
    /// Falls back to forceful termination if graceful shutdown times out.
    ///
    /// Examples:
    ///   mik down                   # Stop "default" instance
    ///   mik down dev               # Stop named instance
    Down {
        /// Instance name to stop (default: "default")
        #[arg(default_value = "default")]
        name: String,
    },
    /// List running WASM instances
    ///
    /// Shows all tracked instances with their status, port, and uptime.
    ///
    /// Examples:
    ///   mik ps                     # List all instances
    Ps,
    /// Show real-time instance statistics
    ///
    /// Displays live CPU and memory usage for running instances.
    /// Updates every second. Press Ctrl+C to exit.
    ///
    /// Examples:
    ///   mik stats                  # Show live stats for all instances
    Stats,
    /// View instance logs
    ///
    /// Shows log output from a running or stopped instance.
    ///
    /// Examples:
    ///   mik logs                   # Show logs for "default" instance
    ///   mik logs dev -f            # Follow logs for named instance
    Logs {
        /// Instance name (default: "default")
        #[arg(default_value = "default")]
        name: String,
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show (default: 50)
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
    },
    /// Show detailed instance information
    ///
    /// Displays full details including configuration and loaded modules.
    ///
    /// Examples:
    ///   mik inspect default        # Inspect default instance
    Inspect {
        /// Instance name to inspect
        name: String,
    },
    /// Remove stopped instances
    ///
    /// Cleans up state for instances that are no longer running.
    ///
    /// Examples:
    ///   mik prune                  # Remove all stopped instances
    Prune,
    /// Manage the AOT compilation cache
    ///
    /// Pre-compiled WASM components are cached in ~/.mik/cache/aot/
    /// using content hashes for fast subsequent loads.
    ///
    /// Examples:
    ///   mik cache info             # Show cache statistics
    ///   mik cache clean            # Remove old entries (LRU)
    ///   mik cache clear            # Remove all cached entries
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    // =========================================================================
    // Build Commands
    // =========================================================================
    /// Create a new mik project
    ///
    /// Scaffolds a complete project with Cargo.toml, mik.toml, WIT files,
    /// and a hello world handler ready to build and run.
    ///
    /// Examples:
    ///   mik new my-service          # Create new project
    ///   mik new my-lib --lib        # Create library component
    New {
        /// Project name (creates directory with this name)
        name: String,
        /// Mark as library component
        #[arg(long)]
        lib: bool,
    },
    /// Add dependencies to mik.toml
    ///
    /// Supports OCI registries (default), HTTP URLs, git repositories, and local paths.
    /// Downloads component from source and updates mik.toml.
    ///
    /// Examples:
    ///   mik add user/repo                    # OCI: ghcr.io/user/repo:latest
    ///   mik add user/repo:v1.0.0             # OCI with specific tag
    ///   mik add ghcr.io/org/package:tag      # Full OCI reference
    ///   mik add <https://example.com/pkg.wasm> # HTTP URL fallback
    ///   mik add user/repo --branch main      # From git branch
    ///   mik add --path ../my-component       # Local development
    #[cfg(feature = "registry")]
    Add {
        /// Packages: user/repo, user/repo:tag, or https://...
        #[arg(required = true)]
        packages: Vec<String>,
        /// Add from git repository
        #[arg(long)]
        git: Option<String>,
        /// Add from local path
        #[arg(long)]
        path: Option<String>,
        /// Registry URL (unused - GitHub is the registry)
        #[arg(long, hide = true)]
        registry: Option<String>,
        /// Git tag
        #[arg(long)]
        tag: Option<String>,
        /// Git branch
        #[arg(long)]
        branch: Option<String>,
        /// Features to enable
        #[arg(short = 'F', long)]
        features: Vec<String>,
        /// Add as dev dependency
        #[arg(short = 'D', long)]
        dev: bool,
    },
    /// Remove dependencies from mik.toml
    ///
    /// Removes component references from manifest and deletes local cache.
    ///
    /// Examples:
    ///   mik remove mik-sdk/router     # Remove production dependency
    ///   mik remove test-utils -D     # Remove dev dependency
    #[cfg(feature = "registry")]
    Remove {
        /// Package names to remove
        #[arg(required = true)]
        packages: Vec<String>,
        /// Remove from dev dependencies
        #[arg(short = 'D', long)]
        dev: bool,
    },
    /// Build the component with cargo-component
    ///
    /// Compiles Rust to WASM component targeting wasm32-wasip2.
    /// Optionally composes all dependencies using WAC.
    ///
    /// Examples:
    ///   mik build                   # Debug build
    ///   mik build --release         # Optimized build
    ///   mik build -rc               # Release + compose dependencies
    Build {
        /// Build in release mode
        #[arg(short, long)]
        release: bool,
        /// Compose all dependencies after build
        #[arg(short, long)]
        compose: bool,
    },
    /// Run the component with local development server
    ///
    /// Starts a WASI HTTP server on <http://127.0.0.1:3000>.
    /// Auto-detects component in target/ if not specified.
    ///
    /// Examples:
    ///   mik run                          # Auto-detect latest build
    ///   mik run target/composed.wasm     # Run specific component
    ///   mik run --workers 4              # Start 4 workers on ports 3000-3003
    ///   mik run --workers 0              # Auto-detect workers (one per CPU)
    Run {
        /// Path to component (default: auto-detect)
        component: Option<String>,

        /// Number of worker processes for horizontal scaling.
        /// Use 0 for auto-detect (one worker per CPU core).
        /// Each worker runs on a separate port (base_port+1, base_port+2, ...).
        /// Use with --lb for integrated load balancer or nginx/caddy/haproxy.
        #[arg(short, long, default_value = "1")]
        workers: u16,

        /// Base port for the load balancer (default: from mik.toml or 3000).
        /// Workers run on ports base_port+1, base_port+2, etc.
        #[arg(short, long)]
        port: Option<u16>,

        /// Bind to localhost only (127.0.0.1) instead of all interfaces (0.0.0.0).
        /// Use for local development when you don't want external access.
        #[arg(short, long)]
        local: bool,

        /// Enable integrated L7 load balancer for multi-worker mode.
        /// The load balancer listens on base_port and distributes requests
        /// to workers using round-robin with health checks.
        #[arg(long)]
        lb: bool,
    },
    /// Publish component to GitHub Container Registry (ghcr.io)
    ///
    /// Builds release component and pushes to ghcr.io.
    /// Requires GitHub authentication (gh auth login).
    ///
    /// Examples:
    ///   mik publish                  # Use version from mik.toml
    ///   mik publish --tag v1.0.0     # Override version tag
    ///   mik publish --dry-run        # Preview without publishing
    #[cfg(feature = "registry")]
    Publish {
        /// Version tag (default: from mik.toml)
        #[arg(long)]
        tag: Option<String>,
        /// Target registry (default: ghcr.io)
        #[arg(long)]
        registry: Option<String>,
        /// Show what would be published without pushing
        #[arg(long)]
        dry_run: bool,
    },
    /// Synchronize dependencies from OCI registries
    ///
    /// Downloads missing dependencies and removes stale modules.
    /// Uses versions specified in mik.toml.
    /// Useful after git clone or switching branches.
    ///
    /// Examples:
    ///   mik sync                     # Sync all dependencies
    #[cfg(feature = "registry")]
    Sync,
    /// Collect static files from component dependencies
    ///
    /// Searches component metadata for static file directories
    /// and copies them to output directory for deployment.
    ///
    /// Examples:
    ///   mik static                   # Extract to ./static
    ///   mik static -o public         # Extract to ./public
    Static {
        /// Output directory (default: static)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Strip debug info and custom sections from WASM components
    ///
    /// Removes debug info, names, and custom sections to reduce file size.
    /// Auto-downloads wasm-tools if not installed.
    ///
    /// Examples:
    ///   mik strip component.wasm                    # Strip all, output to component.stripped.wasm
    ///   mik strip component.wasm -o slim.wasm       # Custom output path
    Strip {
        /// Input WASM component file
        input: String,
        /// Output file path (default: input.stripped.wasm)
        #[arg(short, long)]
        output: Option<String>,
        /// Only remove debug info (.debug* sections)
        #[arg(long)]
        debug_only: bool,
    },
}

#[derive(Subcommand)]
enum CacheAction {
    /// Show cache statistics
    ///
    /// Displays the number of cached entries, total size, and cache location.
    Info,
    /// Remove old cache entries (LRU)
    ///
    /// Removes least recently used entries until cache size is under the limit.
    Clean {
        /// Maximum cache size in MB (default: 1024)
        #[arg(long, default_value = "1024")]
        max_size_mb: u64,
    },
    /// Remove all cached entries
    ///
    /// Clears the entire AOT cache. Components will be recompiled on next load.
    Clear,
}

fn print_verbose_version() {
    println!("mik {}", env!("CARGO_PKG_VERSION"));

    println!("\nKey dependencies:");
    #[cfg(feature = "registry")]
    println!("  clap, serde, serde_json, toml, ureq, git2, oci-client, anyhow");
    #[cfg(not(feature = "registry"))]
    println!("  clap, serde, serde_json, toml, anyhow (minimal build)");
    println!("  (Use 'cargo tree -p mik' for full dependency tree)");

    println!("\nRequired external tools:");
    check_tool("cargo-component", "cargo install cargo-component");
    check_tool("wac", "cargo install wac-cli");
    check_tool("wasm-tools", "cargo install wasm-tools");
}

fn check_tool(name: &str, install_cmd: &str) {
    use std::process::Command;

    let result = Command::new(name).arg("--version").output();

    match result {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let version_line = version.lines().next().unwrap_or("unknown");
            println!("  ✓ {version_line}");
        },
        _ => {
            println!("  ✗ {name} not found (install: {install_cmd})");
        },
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle --version-verbose flag
    if cli.version_verbose {
        print_verbose_version();
        return Ok(());
    }

    // Handle missing subcommand
    let Some(command) = cli.command else {
        eprintln!("Error: A subcommand is required");
        eprintln!("Run 'mik --help' for usage information");
        std::process::exit(1);
    };

    match command {
        // Daemon commands
        Commands::Daemon { port } => {
            commands::daemon::start(port).await?;
        },
        Commands::Up { name, port, watch } => {
            commands::daemon::up(&name, port, watch).await?;
        },
        Commands::Down { name } => {
            commands::daemon::down(&name)?;
        },
        Commands::Ps => {
            commands::daemon::ps()?;
        },
        Commands::Stats => {
            commands::daemon::stats().await?;
        },
        Commands::Logs {
            name,
            follow,
            lines,
        } => {
            commands::daemon::logs(&name, follow, lines).await?;
        },
        Commands::Inspect { name } => {
            commands::daemon::inspect(&name)?;
        },
        Commands::Prune => {
            commands::daemon::prune()?;
        },
        Commands::Cache { action } => {
            commands::cache::execute(action)?;
        },

        // Build commands
        Commands::New { name, lib } => {
            commands::new::execute(&name, lib)?;
        },
        #[cfg(feature = "registry")]
        Commands::Add {
            packages,
            git,
            path,
            registry,
            tag,
            branch,
            features,
            dev,
        } => {
            commands::add::execute(
                &packages,
                git.as_deref(),
                path.as_deref(),
                registry.as_deref(),
                tag.as_deref(),
                branch.as_deref(),
                &features,
                dev,
            )
            .await?;
        },
        #[cfg(feature = "registry")]
        Commands::Remove { packages, dev } => {
            commands::add::remove(&packages, dev)?;
        },
        Commands::Build { release, compose } => {
            commands::build::execute(release, compose).await?;
        },
        Commands::Run {
            component,
            workers,
            port,
            local,
            lb,
        } => {
            commands::run::execute(component.as_deref(), workers, port, local, lb).await?;
        },
        #[cfg(feature = "registry")]
        Commands::Publish {
            tag,
            registry,
            dry_run,
        } => {
            commands::publish::execute(tag.as_deref(), registry.as_deref(), dry_run)?;
        },
        #[cfg(feature = "registry")]
        Commands::Sync => {
            commands::pull::sync().await?;
        },
        Commands::Static { output } => {
            commands::static_cmd::execute(output.as_deref())?;
        },
        Commands::Strip {
            input,
            output,
            debug_only,
        } => {
            let options = commands::strip::StripOptions {
                all: !debug_only,
                debug: debug_only,
                output,
                ..Default::default()
            };
            commands::strip::execute(&input, options)?;
        },
    }

    Ok(())
}
