//! mik - Package Manager and Runtime for WASI HTTP Components.
//!
//! This is the main entry point for the mik CLI. It provides commands for:
//!
//! - Creating new projects (`mik new`)
//! - Building WASM components (`mik build`)
//! - Running development servers (`mik dev`, `mik run`)
//! - Managing background instances (`mik ps`, `mik stop`)
//! - Pulling dependencies from OCI registries (`mik add`, `mik sync`)
//!
//! See `mik --help` for full usage information.

#![allow(clippy::redundant_pub_crate)] // Explicit pub(crate) documents intent, aids refactoring

// Use mimalloc for better multi-core performance (especially important for musl builds)
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Generator, Shell};
use std::str::FromStr;

mod cache;
mod commands;
mod constants;
mod daemon;
mod manifest;
#[cfg(feature = "registry")]
mod registry;
mod reliability;
mod runtime;
mod ui;
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

  # Build for production
  mik build --release --compose

EXAMPLES:
  mik new my-service                Create new project
  mik add user/router:v2.0          Add versioned dependency
  mik build -rc                     Release build with composition
  mik run                           Run on port 3000

For more help, see: https://github.com/dufeutech/mik";

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

    /// Enable verbose/debug output for any command
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    // =========================================================================
    // Development & Runtime
    // =========================================================================
    /// Start development server with watch mode and services
    ///
    /// Runs your WASM handlers with hot reload and embedded services
    /// (KV, SQL, Storage, Cron). Auto-rebuilds on source changes.
    ///
    /// Examples:
    ///   mik dev                    # Watch mode on port 3000
    ///   mik dev --port 8080        # Custom port
    ///   mik dev --no-services      # Skip embedded services
    Dev {
        /// Port for the HTTP server (default: 3000)
        #[arg(short, long, default_value = "3000")]
        port: u16,
        /// Skip starting embedded services (KV, SQL, etc.)
        #[arg(long)]
        no_services: bool,
    },
    // =========================================================================
    // Instance Management
    // =========================================================================
    /// Stop a running instance
    ///
    /// Gracefully stops the instance, waiting for in-flight requests.
    /// Falls back to forceful termination if graceful shutdown times out.
    ///
    /// Examples:
    ///   mik stop                   # Stop "default" instance
    ///   mik stop myapp             # Stop named instance
    Stop {
        /// Instance name to stop (default: "default")
        #[arg(default_value = "default")]
        name: String,
    },
    /// Start the daemon (internal, auto-managed)
    #[command(hide = true)]
    Daemon {
        /// Port for the daemon API (overrides ~/.mik/daemon.toml)
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// List running WASM instances
    ///
    /// Shows all tracked instances with their status, port, and uptime.
    ///
    /// Examples:
    ///   mik ps                     # List all instances
    ///   mik ps --json              # Output as JSON for scripting
    Ps {
        /// Output as JSON for scripting and automation
        #[arg(long)]
        json: bool,
    },
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
    /// Generate shell completions
    ///
    /// Outputs shell completion script to stdout.
    /// Add to your shell config for tab completion support.
    ///
    /// Examples:
    ///   mik completions bash > ~/.bash_completion.d/mik
    ///   mik completions zsh > ~/.zfunc/_mik
    ///   mik completions fish > ~/.config/fish/completions/mik.fish
    ///   mik completions powershell > _mik.ps1
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    // =========================================================================
    // Build Commands
    // =========================================================================
    /// Create a new mik project
    ///
    /// Scaffolds a project from templates supporting multiple languages:
    /// Rust (default) and `TypeScript`.
    ///
    /// Examples:
    ///   mik new my-service                    # Interactive mode
    ///   mik new my-service -y                 # Use defaults (Rust + basic)
    ///   mik new my-service --lang typescript  # `TypeScript` project
    ///   mik new my-api --lang rust --template rest-api
    ///   mik new my-service --template github:user/repo
    New {
        /// Project name (creates directory with this name)
        name: String,
        /// Target language: rust, typescript (ts)
        #[arg(long, short = 'l')]
        lang: Option<String>,
        /// Template: basic (default), rest-api (Rust only), or github:user/repo
        #[arg(long, short = 't')]
        template: Option<String>,
        /// Skip interactive prompts, use defaults
        #[arg(long, short = 'y')]
        yes: bool,
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
        /// Git tag
        #[arg(long)]
        tag: Option<String>,
        /// Git branch
        #[arg(long)]
        branch: Option<String>,
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
    /// Compiles to WASM component targeting `wasm32-wasip2`.
    /// Supports multiple languages: Rust (default) and `TypeScript`.
    /// Optionally composes all dependencies using WAC.
    /// Extracts OpenAPI schema if handler uses mik-sdk routes! macro.
    ///
    /// Examples:
    ///   mik build                   # Build (language from mik.toml or Rust)
    ///   mik build --release         # Optimized build
    ///   mik build -rc               # Release + compose dependencies
    ///   mik build --lang ts         # Build `TypeScript` project
    ///   mik build --no-schema       # Skip schema extraction
    Build {
        /// Build in release mode
        #[arg(short, long)]
        release: bool,
        /// Compose all dependencies after build
        #[arg(short, long)]
        compose: bool,
        /// Language override: rust, typescript (ts)
        #[arg(long, short = 'l', value_parser = ["rust", "rs", "typescript", "ts"])]
        lang: Option<String>,
        /// Skip OpenAPI schema extraction
        #[arg(long)]
        no_schema: bool,
    },
    /// Run the component with local development server
    ///
    /// Starts a WASI HTTP server. Runs in foreground by default.
    /// Use --detach to run as a background instance with services.
    ///
    /// Examples:
    ///   mik run                          # Foreground, auto-detect component
    ///   mik run --detach                 # Background with services
    ///   mik run --detach --name prod     # Named background instance
    ///   mik run --workers 4 --lb         # Multi-worker with load balancer
    Run {
        /// Path to component (default: auto-detect)
        component: Option<String>,

        /// Run as background instance with embedded services (KV, SQL, Cron).
        /// Auto-starts daemon if not running. Use `mik ps` to list instances.
        #[arg(short, long)]
        detach: bool,

        /// Instance name when running detached (default: "default")
        #[arg(short, long, default_value = "default")]
        name: String,

        /// Number of worker processes for horizontal scaling.
        /// Use 0 for auto-detect (one worker per CPU core).
        /// Each worker runs on a separate port (`base_port`+1, `base_port`+2, ...).
        /// Use with --lb for integrated load balancer or nginx/caddy/haproxy.
        #[arg(short, long, default_value = "1")]
        workers: u16,

        /// Base port for the load balancer (default: from `mik.toml` or 3000).
        /// Workers run on ports `base_port`+1, `base_port`+2, etc.
        #[arg(short, long)]
        port: Option<u16>,

        /// Bind to localhost only (127.0.0.1) instead of all interfaces (0.0.0.0).
        /// Use for local development when you don't want external access.
        #[arg(short, long)]
        local: bool,

        /// Enable integrated L7 load balancer for multi-worker mode.
        /// The load balancer listens on `base_port` and distributes requests
        /// to workers using round-robin with health checks.
        #[arg(long)]
        lb: bool,
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

fn print_completions<G: Generator>(generator: G, cmd: &mut clap::Command) {
    clap_complete::generate(
        generator,
        cmd,
        cmd.get_name().to_string(),
        &mut std::io::stdout(),
    );
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
    ui::print_tool_check("cargo-component", "cargo install cargo-component");
    ui::print_tool_check("wac", "cargo install wac-cli");
    ui::print_tool_check("wasm-tools", "cargo install wasm-tools");
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle --version-verbose flag
    if cli.version_verbose {
        print_verbose_version();
        return Ok(());
    }

    // Handle --verbose flag: set RUST_LOG=debug if not already set
    if cli.verbose && std::env::var("RUST_LOG").is_err() {
        // SAFETY: This is called at program startup before any threads are spawned,
        // so there are no concurrent reads/writes to environment variables.
        unsafe { std::env::set_var("RUST_LOG", "debug") };
    }

    // Handle missing subcommand
    let Some(command) = cli.command else {
        eprintln!("Error: A subcommand is required");
        eprintln!("Run 'mik --help' for usage information");
        std::process::exit(1);
    };

    match command {
        // Development
        Commands::Dev { port, no_services } => {
            commands::dev::execute(port, no_services).await?;
        },

        // Instance management
        Commands::Stop { name } => {
            commands::daemon::stop(&name).await?;
        },
        Commands::Daemon { port } => {
            commands::daemon::start(port).await?;
        },
        Commands::Ps { json } => {
            commands::daemon::ps(json)?;
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
        Commands::Completions { shell } => {
            print_completions(shell, &mut Cli::command());
        },

        // Build commands
        Commands::New {
            name,
            lang,
            template,
            yes,
        } => {
            use commands::new::{Language, NewOptions, Template};

            // Parse language
            let lang = lang
                .as_deref()
                .and_then(|s| Language::from_str(s).ok())
                .unwrap_or_default();

            // Determine GitHub template:
            // - Explicit github:user/repo or user/repo → use that
            // - Explicit embedded template (basic, rest-api) → None
            // - No template specified → default GitHub template based on language
            let github_template = if let Some(ref t) = template {
                if t.starts_with("github:") || t.contains('/') {
                    Some(t.clone())
                } else {
                    None // Explicit embedded template
                }
            } else {
                // Default to GitHub template based on language
                Some(match lang {
                    Language::Rust => "dufeutech/mik-handler-template".to_string(),
                    Language::TypeScript => "dufeutech/mik-handler-template-ts".to_string(),
                })
            };

            // Parse template (only if using embedded)
            let template = if github_template.is_none() {
                template
                    .as_deref()
                    .and_then(|s| Template::from_str(s).ok())
                    .unwrap_or_default()
            } else {
                Template::default()
            };

            let options = NewOptions {
                name,
                lang,
                template,
                yes,
                github_template,
            };

            commands::new::execute(options).await?;
        },
        #[cfg(feature = "registry")]
        Commands::Add {
            packages,
            git,
            path,
            tag,
            branch,
            dev,
        } => {
            commands::add::execute(
                &packages,
                git.as_deref(),
                path.as_deref(),
                tag.as_deref(),
                branch.as_deref(),
                dev,
            )
            .await?;
        },
        #[cfg(feature = "registry")]
        Commands::Remove { packages, dev } => {
            commands::add::remove(&packages, dev)?;
        },
        Commands::Build {
            release,
            compose,
            lang,
            no_schema,
        } => {
            commands::build::execute(release, compose, lang, no_schema).await?;
        },
        Commands::Run {
            component,
            detach,
            name,
            workers,
            port,
            local,
            lb,
        } => {
            if detach {
                // Background mode with daemon services
                commands::daemon::run_detached(&name, port.unwrap_or(3000)).await?;
            } else {
                // Foreground mode
                commands::run::execute(component.as_deref(), workers, port, local, lb).await?;
            }
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
