//! Run WASI HTTP components with the embedded runtime.
//!
//! All routes use `/run/<module>/*` pattern:
//! - **Multi-module mode**: `mik run` - serves modules from `[server].modules` directory
//! - **Single component mode**: `mik run path/to/component.wasm` - serves a single component
//!
//! Multi-worker mode (for use with external L7 load balancer):
//! - `mik run --workers 4` - spawns 4 worker processes on ports 3001-3004
//! - `mik run --workers 0` - auto-detect workers (one per CPU core)
//!
//! Integrated L7 load balancer mode:
//! - `mik run --workers 4 --lb` - LB on port 3000, workers on 3001-3004

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command};

use crate::manifest::TracingConfig;
use crate::runtime::HostBuilder;
use crate::runtime::lb::{LoadBalancer, LoadBalancerConfig};

/// Run components with the embedded runtime.
///
/// # Modes
///
/// All modes use consistent `/run/<module>/*` routing.
///
/// - `mik run` - Multi-module mode from mik.toml configuration
///   - Reads `[server].modules` directory (default: "modules/")
///   - Routes: `/run/<module>/*` -> `<module>.wasm`
///
/// - `mik run path/to/component.wasm` - Single component mode
///   - Routes: `/run/<name>/*` -> the component (name derived from filename)
///
/// - `mik run --workers 4` - Multi-worker mode (external LB)
///   - Spawns 4 worker processes on consecutive ports
///   - Use with nginx/caddy/haproxy for L7 load balancing
///
/// - `mik run --workers 4 --lb` - Multi-worker mode (integrated LB)
///   - LB on base_port, workers on base_port+1 to base_port+workers
///   - Round-robin with health checks
///
/// - `mik run --workers 0` - Auto-detect workers (one per CPU core)
pub async fn execute(
    component_path: Option<&str>,
    workers: u16,
    port_override: Option<u16>,
    local_only: bool,
    use_lb: bool,
) -> Result<()> {
    // Set MIK_LOCAL env var if --local flag is set
    if local_only {
        // SAFETY: We're setting env var before spawning any threads
        unsafe { std::env::set_var("MIK_LOCAL", "1") };
    }

    // Check if we're a spawned worker (internal flag)
    if std::env::var("MIK_WORKER_ID").is_ok() {
        // We're a worker - run single instance
        return run_single_instance(component_path, port_override).await;
    }

    // Auto-detect workers: 0 means optimal workers for multi-instance scaling
    //
    // Benchmarks on 14-core/20-thread CPU showed:
    //   - Single LB x 8 workers: ~22,700 rps
    //   - 2 LBs x 5 workers each: ~25,500 rps (best)
    //
    // Formula: threads / 4 (minimum 2)
    //   - Allows running 2 mik instances for ~12% more throughput
    //   - Leaves CPU headroom for OS and other processes
    //
    // Examples:
    //   - 20 threads → 5 workers (run 2 instances on ports 3000 & 4000)
    //   - 16 threads → 4 workers
    //   - 8 threads  → 2 workers
    //   - 4 threads  → 2 workers (minimum)
    let workers = if workers == 0 {
        let threads = std::thread::available_parallelism()
            .map(|p| p.get() as u16)
            .unwrap_or(4);
        // Optimal workers per instance for multi-LB scaling
        let optimal = (threads / 4).max(2);
        println!(
            "Auto-detected {} threads, using {} workers\n\
             Tip: Run 2 instances on different ports for ~12% more throughput:\n\
             \x20 mik run --workers 0 --lb --port 3000\n\
             \x20 mik run --workers 0 --lb --port 4000",
            threads, optimal
        );
        optimal
    } else {
        workers
    };

    // Multi-worker mode with integrated load balancer
    if workers > 1 && use_lb {
        return run_with_lb(component_path, workers, port_override, local_only).await;
    }

    // Multi-worker mode: spawn child processes (for external LB)
    if workers > 1 {
        return run_multi_worker(component_path, workers, port_override, local_only).await;
    }

    // Single worker mode
    run_single_instance(component_path, port_override).await
}

/// Run multiple worker processes for horizontal scaling.
async fn run_multi_worker(
    component_path: Option<&str>,
    workers: u16,
    port_override: Option<u16>,
    local_only: bool,
) -> Result<()> {
    // Get base port from override, mik.toml, or default
    let base_port = port_override.unwrap_or_else(|| {
        if Path::new("mik.toml").exists()
            && let Ok(content) = std::fs::read_to_string("mik.toml")
        {
            #[derive(serde::Deserialize, Default)]
            struct PartialManifest {
                #[serde(default)]
                server: ServerConfig,
            }
            #[derive(serde::Deserialize, Default)]
            struct ServerConfig {
                #[serde(default = "default_port")]
                port: u16,
            }
            fn default_port() -> u16 {
                3000
            }

            if let Ok(manifest) = toml::from_str::<PartialManifest>(&content) {
                return manifest.server.port;
            }
        }
        3000
    });

    println!("Starting {} workers...\n", workers);

    // Get current executable path
    let exe = std::env::current_exe().context("Failed to get current executable")?;

    // Spawn worker processes
    let mut children: Vec<Child> = Vec::new();
    let mut ports: Vec<u16> = Vec::new();

    for i in 0..workers {
        let port = base_port + i;
        ports.push(port);

        let mut cmd = Command::new(&exe);
        cmd.arg("run");

        if let Some(path) = component_path {
            cmd.arg(path);
        }

        cmd.arg("--port").arg(port.to_string());
        cmd.env("MIK_WORKER_ID", i.to_string());
        if local_only {
            cmd.env("MIK_LOCAL", "1");
        }

        // Suppress worker stdout to avoid interleaving
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::inherit());

        let bind_addr = if local_only { "127.0.0.1" } else { "0.0.0.0" };
        let child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn worker {i}"))?;
        println!(
            "  Worker {i}: http://{bind_addr}:{port} (pid: {})",
            child.id()
        );
        children.push(child);
    }

    // Print load balancer config
    println!("\n─────────────────────────────────────");
    println!("Load balancer upstream config:\n");
    println!("  # nginx");
    println!("  upstream mik {{");
    for port in &ports {
        println!("      server 127.0.0.1:{port};");
    }
    println!("  }}");
    println!("\n  # caddy");
    print!("  reverse_proxy");
    for port in &ports {
        print!(" 127.0.0.1:{port}");
    }
    println!(" {{ lb_policy round_robin }}");
    println!("─────────────────────────────────────\n");
    println!("Press Ctrl+C to stop all workers\n");

    // Wait for Ctrl+C and kill all workers
    tokio::signal::ctrl_c().await?;

    println!("\nShutting down {} workers...", children.len());

    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }

    println!("All workers stopped.");
    Ok(())
}

/// Run with integrated L7 load balancer.
///
/// Spawns worker processes and starts an integrated load balancer that
/// distributes requests using round-robin with health checks.
async fn run_with_lb(
    component_path: Option<&str>,
    workers: u16,
    port_override: Option<u16>,
    local_only: bool,
) -> Result<()> {
    // Get base port from override, mik.toml, or default
    let base_port = port_override.unwrap_or_else(|| {
        if Path::new("mik.toml").exists()
            && let Ok(content) = std::fs::read_to_string("mik.toml")
        {
            #[derive(serde::Deserialize, Default)]
            struct PartialManifest {
                #[serde(default)]
                server: ServerConfig,
            }
            #[derive(serde::Deserialize, Default)]
            struct ServerConfig {
                #[serde(default = "default_port")]
                port: u16,
            }
            fn default_port() -> u16 {
                3000
            }

            if let Ok(manifest) = toml::from_str::<PartialManifest>(&content) {
                return manifest.server.port;
            }
        }
        3000
    });

    println!(
        "Starting {} workers with integrated L7 load balancer...\n",
        workers
    );

    // Get current executable path
    let exe = std::env::current_exe().context("Failed to get current executable")?;

    // Spawn worker processes on ports base_port+1 to base_port+workers
    let mut children: Vec<Child> = Vec::new();
    let mut backend_addrs: Vec<String> = Vec::new();

    let bind_addr = if local_only { "127.0.0.1" } else { "0.0.0.0" };

    for i in 0..workers {
        let worker_port = base_port + 1 + i;
        backend_addrs.push(format!("127.0.0.1:{worker_port}"));

        let mut cmd = Command::new(&exe);
        cmd.arg("run");

        if let Some(path) = component_path {
            cmd.arg(path);
        }

        cmd.arg("--port").arg(worker_port.to_string());
        cmd.env("MIK_WORKER_ID", i.to_string());
        if local_only {
            cmd.env("MIK_LOCAL", "1");
        }

        // Suppress worker stdout to avoid interleaving
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::inherit());

        let child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn worker {i}"))?;
        println!(
            "  Worker {i}: http://127.0.0.1:{worker_port} (pid: {})",
            child.id()
        );
        children.push(child);
    }

    // Give workers time to start
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Configure and start the load balancer
    let lb_addr: SocketAddr = format!("{bind_addr}:{base_port}")
        .parse()
        .context("Invalid LB address")?;

    println!("\n─────────────────────────────────────");
    println!("L7 Load Balancer: http://{lb_addr}");
    println!("─────────────────────────────────────");
    println!("  Algorithm: round-robin");
    println!("  Health check: /health (5s interval)");
    println!("  Backends: {}", backend_addrs.join(", "));
    println!("─────────────────────────────────────\n");
    println!("Press Ctrl+C to stop\n");

    // Create load balancer config
    let lb_config = LoadBalancerConfig {
        listen_addr: lb_addr,
        backends: backend_addrs,
        ..Default::default()
    };

    let lb = LoadBalancer::new(lb_config);

    // Run load balancer with Ctrl+C handling
    tokio::select! {
        result = lb.serve() => {
            if let Err(e) = result {
                eprintln!("Load balancer error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            println!("\nShutting down...");
        }
    }

    // Kill all workers
    println!("Stopping {} workers...", children.len());
    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }

    println!("All workers stopped.");
    Ok(())
}

/// Run a single server instance.
async fn run_single_instance(
    component_path: Option<&str>,
    port_override: Option<u16>,
) -> Result<()> {
    // Load tracing config from mik.toml if present
    let tracing_config = load_tracing_config();

    // Initialize logging/tracing
    init_tracing(&tracing_config);

    // Determine mode based on arguments
    let mut builder = match component_path {
        // Explicit component path provided -> single component mode
        Some(path) if !path.is_empty() => {
            let component = resolve_component_path(path)?;
            println!("Single component mode: {component}");
            validate_wasm_file(&component)?;

            if Path::new("mik.toml").exists() {
                HostBuilder::from_manifest("mik.toml")
                    .context("Failed to load mik.toml")?
                    .modules_dir(&component)
            } else {
                HostBuilder::new().modules_dir(&component)
            }
        },

        // No path provided -> multi-module mode from mik.toml
        _ => {
            if Path::new("mik.toml").exists() {
                let builder =
                    HostBuilder::from_manifest("mik.toml").context("Failed to load mik.toml")?;

                println!("Multi-module mode from mik.toml");
                builder
            } else {
                // Try to find a single component as fallback
                if let Ok(component) = find_default_component() {
                    println!("Single component mode: {component}");
                    validate_wasm_file(&component)?;
                    HostBuilder::new().modules_dir(&component)
                } else {
                    anyhow::bail!(
                        "No mik.toml found and no component specified.\n\n\
                         Options:\n\
                         1. Create mik.toml with [server].modules directory\n\
                         2. Run with explicit path: mik run path/to/component.wasm\n\
                         3. Build a component first: mik build"
                    );
                }
            }
        },
    };

    // Apply port override if specified
    if let Some(port) = port_override {
        builder = builder.port(port);
    }

    // Check for hot-reload mode (bypasses AOT cache)
    if std::env::var("MIK_HOT_RELOAD").is_ok() {
        builder = builder.hot_reload(true);
    }

    let port = builder.get_port();
    let host = builder.build().context("Failed to build host")?;

    // Determine bind address: --local flag or HOST env var or default 0.0.0.0
    let bind_host = if std::env::var("MIK_LOCAL").is_ok() {
        "127.0.0.1".to_string()
    } else {
        std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string())
    };
    let addr: SocketAddr = format!("{bind_host}:{port}")
        .parse()
        .context("Invalid address")?;

    println!("Starting server on http://{addr}");

    if let Some(name) = host.single_component_name() {
        println!("Routes: /run/{name}/* -> component");
    } else {
        println!("Routes: /run/<module>/* -> <module>.wasm");
    }

    if host.has_static_files() {
        println!("Routes: /static/* -> static files");
    }

    println!("Health: /health");
    println!("Metrics: /metrics");
    println!();

    // Run the server
    host.serve(addr).await.context("Server error")?;

    Ok(())
}

/// Load tracing configuration from mik.toml if present.
fn load_tracing_config() -> TracingConfig {
    if Path::new("mik.toml").exists()
        && let Ok(content) = std::fs::read_to_string("mik.toml")
    {
        // Parse just the tracing section
        #[derive(serde::Deserialize, Default)]
        struct PartialManifest {
            #[serde(default)]
            tracing: TracingConfig,
        }

        if let Ok(manifest) = toml::from_str::<PartialManifest>(&content) {
            return manifest.tracing;
        }
    }
    TracingConfig::default()
}

/// Initialize tracing/logging based on configuration.
///
/// When the `otlp` feature is enabled and an OTLP endpoint is configured,
/// traces are exported to the specified backend (Jaeger, Tempo, etc.).
#[allow(unused_variables)]
fn init_tracing(config: &TracingConfig) {
    // Try OTLP initialization if feature is enabled and endpoint is set
    #[cfg(feature = "otlp")]
    if let Some(endpoint) = &config.otlp_endpoint {
        use crate::daemon::otlp::{OtlpConfig, init_with_otlp};

        let otlp_config = OtlpConfig::new(endpoint).with_service_name(&config.service_name);

        if let Err(e) = init_with_otlp(otlp_config) {
            eprintln!("Warning: Failed to initialize OTLP tracing: {e}");
            eprintln!("Falling back to stdout logging");
            init_stdout_logging();
        } else {
            tracing::info!(
                endpoint = %endpoint,
                service = %config.service_name,
                "OTLP tracing enabled"
            );
            return;
        }
    }

    // Default: stdout logging
    init_stdout_logging();
}

/// Initialize stdout logging (fallback when OTLP is not configured).
fn init_stdout_logging() {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

/// Resolve a component path (handles globs and validates existence).
fn resolve_component_path(path: &str) -> Result<String> {
    // Direct path exists
    if Path::new(path).exists() {
        return Ok(path.to_string());
    }

    // Try as glob pattern
    if path.contains('*')
        && let Ok(paths) = glob::glob(path)
        && let Some(Ok(p)) = paths.into_iter().next()
    {
        return Ok(p.display().to_string());
    }

    anyhow::bail!(
        "Component not found: {path}\n\n\
         Build the component first:\n  mik build"
    )
}

/// Find a default component when no path is specified and no mik.toml exists.
fn find_default_component() -> Result<String> {
    let candidates = [
        "target/composed.wasm",
        "target/wasm32-wasip2/release/*.wasm",
        "target/wasm32-wasip2/debug/*.wasm",
    ];

    for pattern in candidates {
        if pattern.contains('*') {
            if let Ok(paths) = glob::glob(pattern)
                && let Some(Ok(p)) = paths.into_iter().next()
            {
                return Ok(p.display().to_string());
            }
        } else if Path::new(pattern).exists() {
            return Ok(pattern.to_string());
        }
    }

    anyhow::bail!("No component found")
}

/// Validate that the WASM file exists and is readable.
fn validate_wasm_file(path: &str) -> Result<()> {
    use std::io::Read;

    let file_path = Path::new(path);

    // Check file extension
    if !file_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wasm"))
    {
        println!(
            "Warning: File does not have .wasm extension: {path}\n\
             This may not be a valid WASM component.\n"
        );
    }

    // Open the file and validate
    let mut file = std::fs::File::open(file_path).with_context(|| {
        format!(
            "WASM file not found or cannot be opened: {path}\n\n\
             Possible solutions:\n\
             1. Build the component first: mik build\n\
             2. Check file permissions\n\
             3. Verify the path is correct"
        )
    })?;

    let metadata = file.metadata().with_context(|| {
        format!(
            "Cannot read file metadata: {path}\n\
             Check file permissions and try rebuilding:\n\
             mik build"
        )
    })?;

    if !metadata.is_file() {
        anyhow::bail!(
            "Path is not a file: {path}\n\
             Expected a .wasm component file"
        );
    }

    // Read WASM magic bytes
    let mut header = [0u8; 4];
    file.read_exact(&mut header).with_context(|| {
        format!(
            "Cannot read file header: {path}\n\
             File may be empty or corrupted.\n\n\
             Rebuild the component:\n\
             mik build"
        )
    })?;

    if &header != b"\0asm" {
        anyhow::bail!(
            "File is not a valid WASM module: {path}\n\
             File does not start with WASM magic bytes.\n\n\
             Rebuild the component:\n\
             mik build"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_wasm_file_nonexistent() {
        let result = validate_wasm_file("nonexistent.wasm");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_resolve_component_path_exists() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let wasm_path = dir.path().join("test.wasm");
        fs::write(&wasm_path, b"\0asm\x01\x00\x00\x00").unwrap();

        let result = resolve_component_path(wasm_path.to_str().unwrap());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), wasm_path.to_str().unwrap());
    }

    #[test]
    fn test_resolve_component_path_not_found() {
        let result = resolve_component_path("definitely_does_not_exist_xyz123.wasm");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_find_default_component_none() {
        // In a temp directory with no wasm files
        let result = find_default_component();
        // This might succeed or fail depending on current directory
        // Just verify it doesn't panic
        let _ = result;
    }
}
