//! CLI command handlers for daemon operations.
//!
//! Implements Docker-like commands for managing WASM instances:
//! api, up, down, ps, stats, logs, inspect, prune.

use anyhow::{Context, Result};
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::daemon::config::DaemonConfig;
use crate::daemon::process::{self, SpawnConfig};
use crate::daemon::state::{Instance, StateStore, Status};

/// Default state database path: ~/.mik/state.redb
fn get_state_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to get home directory")?;
    Ok(home.join(".mik").join("state.redb"))
}

/// Start the daemon for process management and scheduling.
///
/// Listens for management requests on the specified port.
/// If port is None, uses the port from daemon config or default (9919).
pub async fn start(port: Option<u16>) -> Result<()> {
    // Load daemon configuration
    let config = DaemonConfig::load().unwrap_or_else(|e| {
        eprintln!("Warning: Failed to load daemon config: {e}");
        eprintln!("Using default configuration");
        DaemonConfig::default()
    });

    // Use CLI port if provided, otherwise config port
    let port = port.unwrap_or(config.daemon.port);

    println!("Starting mik daemon on port {port}...");
    println!("API endpoint: http://127.0.0.1:{port}");
    println!("\nEndpoints:");
    println!(
        "  Instances:  /instances, /instances/:name, /instances/:name/restart, /instances/:name/logs"
    );
    println!("  Cron:       /cron, /cron/:name, /cron/:name/trigger, /cron/:name/history");

    // Show service status
    print!("  Services:   ");
    let mut services = Vec::new();
    if config.services.kv_enabled {
        services.push("/kv");
    }
    if config.services.sql_enabled {
        services.push("/sql");
    }
    if config.services.storage_enabled {
        services.push("/storage");
    }
    if services.is_empty() {
        println!("(all disabled)");
    } else {
        println!("{}", services.join(", "));
    }

    println!("  System:     /health, /version, /metrics");

    // Show disabled services warning
    if !config.services.kv_enabled
        || !config.services.sql_enabled
        || !config.services.storage_enabled
    {
        println!("\nDisabled services:");
        if !config.services.kv_enabled {
            println!("  - KV service");
        }
        if !config.services.sql_enabled {
            println!("  - SQL service");
        }
        if !config.services.storage_enabled {
            println!("  - Storage service");
        }
        println!("  Edit ~/.mik/daemon.toml to enable them");
    }

    println!("\nPress Ctrl+C to stop the daemon\n");

    let state_path = get_state_path()?;
    crate::daemon::http::serve(port, state_path, config).await
}

/// Stop a running WASM instance.
///
/// Gracefully terminates the process and updates state.
/// Auto-exits daemon if this was the last running instance.
pub async fn stop(name: &str) -> Result<()> {
    let state_path = get_state_path()?;
    let store = StateStore::open(&state_path)?;

    let instance = store
        .get_instance(name)?
        .with_context(|| format!("Instance '{name}' not found"))?;

    if instance.status != Status::Running {
        println!(
            "Instance '{}' is not running (status: {:?})",
            name, instance.status
        );
        return Ok(());
    }

    println!("Stopping instance '{name}'...");

    // Kill the process
    if process::is_running(instance.pid)? {
        process::kill_instance(instance.pid)?;
    }

    // Update state
    let mut updated = instance;
    updated.status = Status::Stopped;
    store.save_instance(&updated)?;

    println!("Instance '{name}' stopped");

    // Check if daemon should auto-exit (no more running instances)
    check_daemon_auto_exit(&store)?;

    Ok(())
}

/// Check if daemon should auto-exit when no instances are running.
fn check_daemon_auto_exit(store: &StateStore) -> Result<()> {
    let instances = store.list_instances()?;
    let running_count = instances
        .iter()
        .filter(|i| i.status == Status::Running && process::is_running(i.pid).unwrap_or(false))
        .count();

    if running_count == 0 {
        // No running instances, stop daemon if running
        if let Some(daemon_pid) = get_daemon_pid() {
            println!("No running instances, stopping daemon...");
            process::kill_instance(daemon_pid)?;
        }
    }

    Ok(())
}

/// Get daemon PID if running.
fn get_daemon_pid() -> Option<u32> {
    let state_path = get_state_path().ok()?;
    let daemon_pid_path = state_path.parent()?.join("daemon.pid");
    std::fs::read_to_string(daemon_pid_path)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Run instance in detached mode with auto-daemon.
///
/// Auto-starts daemon if not running, then creates the instance.
pub async fn run_detached(name: &str, port: u16) -> Result<()> {
    // Ensure daemon is running
    ensure_daemon_running().await?;

    // Now start the instance (reuse up logic)
    let state_path = get_state_path()?;
    let store = StateStore::open(&state_path)?;

    // Check if instance already exists and is running
    if let Some(existing) = store.get_instance(name)?
        && existing.status == Status::Running
        && process::is_running(existing.pid)?
    {
        println!(
            "Instance '{}' is already running on port {}",
            name, existing.port
        );
        return Ok(());
    }

    // Find mik.toml in current directory
    let config_path = std::env::current_dir()?.join("mik.toml");
    if !config_path.exists() {
        anyhow::bail!("No mik.toml found in current directory.");
    }

    let working_dir = std::env::current_dir()?;

    println!("Starting instance '{name}' on port {port}...");

    let spawn_config = SpawnConfig {
        name: name.to_string(),
        port,
        config_path: config_path.clone(),
        working_dir: working_dir.clone(),
        hot_reload: false,
    };

    let info = process::spawn_instance(&spawn_config)?;

    // Save instance state
    let instance = Instance {
        name: name.to_string(),
        port,
        pid: info.pid,
        status: Status::Running,
        config: config_path.clone(),
        started_at: Utc::now(),
        modules: vec![],
        auto_restart: false,
        restart_count: 0,
        last_restart_at: None,
    };

    store.save_instance(&instance)?;

    println!("Instance '{}' started (PID: {})", name, info.pid);
    println!("Logs: {}", info.log_path.display());
    println!("\nAccess at: http://127.0.0.1:{port}");
    println!("\nManage with:");
    println!("  mik ps          # List instances");
    println!("  mik logs {name}   # View logs");
    println!("  mik stop {name}   # Stop instance");

    Ok(())
}

/// Ensure daemon is running, auto-start if not.
async fn ensure_daemon_running() -> Result<()> {
    const DAEMON_PORT: u16 = 9919;

    // Check if daemon is already running
    if is_daemon_running(DAEMON_PORT).await {
        return Ok(());
    }

    println!("Starting daemon...");

    // Get path to mik executable
    let mik_exe = std::env::current_exe()?;

    // Spawn daemon in background
    let state_path = get_state_path()?;
    let daemon_log = state_path.parent().unwrap().join("logs").join("daemon.log");
    std::fs::create_dir_all(daemon_log.parent().unwrap())?;

    let log_file = std::fs::File::create(&daemon_log)?;

    let child = std::process::Command::new(&mik_exe)
        .args(["daemon", "--port", &DAEMON_PORT.to_string()])
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .context("Failed to start daemon")?;

    // Save daemon PID
    let daemon_pid_path = state_path.parent().unwrap().join("daemon.pid");
    std::fs::write(&daemon_pid_path, child.id().to_string())?;

    // Wait for daemon to be ready
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if is_daemon_running(DAEMON_PORT).await {
            println!("Daemon started (PID: {})", child.id());
            return Ok(());
        }
    }

    anyhow::bail!("Daemon failed to start within 5 seconds")
}

/// Check if daemon is running by trying to connect.
async fn is_daemon_running(port: u16) -> bool {
    reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/health"))
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .is_ok()
}

/// List all tracked WASM instances.
///
/// Shows status, port, PID, and uptime for each instance.
pub fn ps() -> Result<()> {
    let state_path = get_state_path()?;

    // Check if state file exists
    if !state_path.exists() {
        println!("No instances found. Start one with 'mik run --detach' or 'mik dev'");
        return Ok(());
    }

    let store = StateStore::open(&state_path)?;
    let instances = store.list_instances()?;

    if instances.is_empty() {
        println!("No instances found. Start one with 'mik run --detach' or 'mik dev'");
        return Ok(());
    }

    // Print header
    println!(
        "{:<12} {:<6} {:<8} {:<10} UPTIME",
        "NAME", "PORT", "PID", "STATUS"
    );
    println!("{}", "─".repeat(60));

    for instance in instances {
        // Check if actually running
        let actual_status = if instance.status == Status::Running {
            if process::is_running(instance.pid)? {
                "running"
            } else {
                "crashed"
            }
        } else {
            match &instance.status {
                Status::Stopped => "stopped",
                Status::Crashed { .. } => "crashed",
                Status::Running => "running",
            }
        };

        let uptime = if actual_status == "running" {
            let duration = Utc::now().signed_duration_since(instance.started_at);
            format_duration(duration)
        } else {
            "-".to_string()
        };

        println!(
            "{:<12} {:<6} {:<8} {:<10} {}",
            instance.name, instance.port, instance.pid, actual_status, uptime
        );
    }

    Ok(())
}

/// Show real-time CPU and memory statistics.
///
/// Continuously updates until Ctrl+C is pressed.
pub async fn stats() -> Result<()> {
    let state_path = get_state_path()?;

    if !state_path.exists() {
        println!("No instances found. Start one with 'mik run --detach' or 'mik dev'");
        return Ok(());
    }

    let store = StateStore::open(&state_path)?;

    println!("Press Ctrl+C to exit\n");

    loop {
        // Clear screen and move cursor to top
        print!("\x1B[2J\x1B[H");

        let instances = store.list_instances()?;

        if instances.is_empty() {
            println!("No instances found.");
        } else {
            // Print header
            println!(
                "{:<12} {:<6} {:<8} {:<8} {:<12} STATUS",
                "NAME", "PORT", "PID", "CPU", "MEM"
            );
            println!("{}", "─".repeat(70));

            let mut system = System::new();
            system.refresh_processes(ProcessesToUpdate::All, true);

            for instance in &instances {
                let (cpu, mem, status) = system.process(Pid::from(instance.pid as usize)).map_or(
                    ("-".to_string(), "-".to_string(), "stopped".to_string()),
                    |proc| {
                        let cpu = format!("{:.1}%", proc.cpu_usage());
                        let mem = format_bytes(proc.memory());
                        (cpu, mem, "running".to_string())
                    },
                );

                println!(
                    "{:<12} {:<6} {:<8} {:<8} {:<12} {}",
                    instance.name, instance.port, instance.pid, mem, cpu, status
                );
            }
        }

        // Wait 1 second before refresh
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(1)) => {},
            _ = tokio::signal::ctrl_c() => {
                println!("\n");
                break;
            }
        }
    }

    Ok(())
}

/// View logs for an instance.
///
/// Shows recent log lines or follows in real-time.
pub async fn logs(name: &str, follow: bool, lines: usize) -> Result<()> {
    let home = dirs::home_dir().context("Failed to get home directory")?;
    let log_path = home.join(".mik").join("logs").join(format!("{name}.log"));

    if !log_path.exists() {
        println!("No logs found for instance '{name}'");
        println!("Log path: {}", log_path.display());
        return Ok(());
    }

    if follow {
        // Follow mode - tail -f style
        println!("Following logs for '{name}' (Ctrl+C to exit)...\n");

        // Use tail command for simplicity
        #[cfg(unix)]
        {
            use std::process::Command;
            let mut child = Command::new("tail")
                .args(["-f", "-n", &lines.to_string()])
                .arg(&log_path)
                .spawn()
                .context("Failed to spawn tail command")?;

            // Wait for Ctrl+C
            tokio::signal::ctrl_c().await?;
            let _ = child.kill();
        }

        #[cfg(windows)]
        {
            // Windows: simple polling approach
            use std::io::{BufRead, BufReader, Seek, SeekFrom};
            let mut file = std::fs::File::open(&log_path)?;
            file.seek(SeekFrom::End(0))?;
            let mut reader = BufReader::new(file);

            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        // No new data, wait a bit
                        tokio::select! {
                            () = tokio::time::sleep(Duration::from_millis(100)) => {},
                            _ = tokio::signal::ctrl_c() => break,
                        }
                    },
                    Ok(_) => print!("{line}"),
                    Err(e) => {
                        eprintln!("Error reading log: {e}");
                        break;
                    },
                }
            }
        }
    } else {
        // Show last N lines
        let log_lines = process::tail_log(&log_path, lines)?;

        if log_lines.is_empty() {
            println!("Log file is empty for instance '{name}'");
        } else {
            for line in log_lines {
                println!("{line}");
            }
        }
    }

    Ok(())
}

/// Show detailed information about an instance.
///
/// Displays configuration, modules, and statistics.
pub fn inspect(name: &str) -> Result<()> {
    let state_path = get_state_path()?;
    let store = StateStore::open(&state_path)?;

    let instance = store
        .get_instance(name)?
        .with_context(|| format!("Instance '{name}' not found"))?;

    // Check actual running status
    let actual_status = if instance.status == Status::Running {
        if process::is_running(instance.pid)? {
            "running"
        } else {
            "crashed (process not found)"
        }
    } else {
        match &instance.status {
            Status::Stopped => "stopped",
            Status::Crashed { exit_code } => &format!("crashed (exit code: {exit_code})"),
            Status::Running => "running",
        }
    };

    let uptime = if actual_status == "running" {
        let duration = Utc::now().signed_duration_since(instance.started_at);
        format_duration(duration)
    } else {
        "-".to_string()
    };

    println!("Name:       {}", instance.name);
    println!("Port:       {}", instance.port);
    println!("PID:        {}", instance.pid);
    println!("Status:     {actual_status}");
    println!(
        "Started:    {}",
        instance.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!("Uptime:     {uptime}");
    println!("Config:     {}", instance.config.display());

    if !instance.modules.is_empty() {
        println!("Modules:");
        for module in &instance.modules {
            println!("  - {module}");
        }
    }

    // Show resource usage if running
    if actual_status == "running" {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);

        if let Some(proc) = system.process(Pid::from(instance.pid as usize)) {
            println!("\nResources:");
            println!("  CPU:      {:.1}%", proc.cpu_usage());
            println!("  Memory:   {}", format_bytes(proc.memory()));
        }
    }

    Ok(())
}

/// Remove stopped instances from state.
///
/// Cleans up instances that are no longer running.
pub fn prune() -> Result<()> {
    let state_path = get_state_path()?;

    if !state_path.exists() {
        println!("No instances to prune");
        return Ok(());
    }

    let store = StateStore::open(&state_path)?;
    let instances = store.list_instances()?;

    let mut pruned = 0;

    for instance in instances {
        let should_prune = match &instance.status {
            Status::Stopped | Status::Crashed { .. } => true,
            Status::Running => !process::is_running(instance.pid)?,
        };

        if should_prune {
            store.remove_instance(&instance.name)?;
            println!("Removed: {}", instance.name);
            pruned += 1;
        }
    }

    if pruned == 0 {
        println!("No stopped instances to prune");
    } else {
        println!("\nPruned {pruned} instance(s)");
    }

    Ok(())
}

// =============================================================================
// Helper Functions
// =============================================================================

// Use shared utilities for duration and bytes formatting
use crate::utils::{format_bytes, format_duration};
