//! CLI command handlers for daemon operations.
//!
//! Implements Docker-like commands for managing WASM instances:
//! api, up, down, ps, stats, logs, inspect, prune.

use anyhow::{Context, Result};
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use sysinfo::{Pid, ProcessesToUpdate, System};

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
pub async fn start(port: u16) -> Result<()> {
    println!("Starting mik daemon on port {port}...");
    println!("API endpoint: http://127.0.0.1:{port}");
    println!("\nEndpoints:");
    println!(
        "  Instances:  /instances, /instances/:name, /instances/:name/restart, /instances/:name/logs"
    );
    println!("  Cron:       /cron, /cron/:name, /cron/:name/trigger, /cron/:name/history");
    println!("  Services:   /services, /services/:name, /services/:name/heartbeat");
    println!("  System:     /health, /version, /metrics");
    println!("\nPress Ctrl+C to stop the daemon\n");

    let state_path = get_state_path()?;
    crate::daemon::http::serve(port, state_path).await
}

/// Start a WASM instance in the background.
///
/// Spawns a new mik server process and tracks it in state.
/// If `watch` is true, runs in foreground with hot reload enabled.
pub async fn up(name: &str, port: u16, watch: bool) -> Result<()> {
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
        anyhow::bail!(
            "No mik.toml found in current directory. Run 'mik init' first or specify --config"
        );
    }

    let working_dir = std::env::current_dir()?;

    println!("Starting instance '{name}' on port {port}...");

    let spawn_config = SpawnConfig {
        name: name.to_string(),
        port,
        config_path: config_path.clone(),
        working_dir: working_dir.clone(),
        hot_reload: false, // AOT cache always enabled (content-addressed, works in watch mode)
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
        modules: vec![], // Will be populated when we read mik.toml
        auto_restart: false,
        restart_count: 0,
        last_restart_at: None,
    };

    store.save_instance(&instance)?;

    println!("Instance '{}' started (PID: {})", name, info.pid);
    println!("Logs: {}", info.log_path.display());
    println!("\nAccess at: http://127.0.0.1:{port}");

    // If watch mode is enabled, start file watcher
    if watch {
        run_watch_mode(
            name,
            &working_dir,
            &config_path,
            spawn_config,
            info.pid,
            &store,
            instance,
        )
        .await?;
    }

    Ok(())
}

/// Run watch mode for hot reloading.
///
/// Monitors modules directory and config file for changes, restarting the instance as needed.
async fn run_watch_mode(
    name: &str,
    working_dir: &std::path::Path,
    config_path: &std::path::Path,
    spawn_config: SpawnConfig,
    initial_pid: u32,
    store: &StateStore,
    instance: Instance,
) -> Result<()> {
    let modules_dir = working_dir.join("modules");

    // Ensure modules directory exists
    if !modules_dir.exists() {
        std::fs::create_dir_all(&modules_dir)?;
    }

    println!();

    // Track current instance for restart
    let current_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(initial_pid));
    let pid_clone = current_pid.clone();
    let name_clone = name.to_string();
    let spawn_config_clone = spawn_config.clone();

    crate::daemon::watch::watch_loop(&modules_dir, config_path, move |event| {
        handle_watch_event(event, &pid_clone, &name_clone, &spawn_config_clone);
    })
    .await?;

    // Clean up on exit
    let final_pid = current_pid.load(std::sync::atomic::Ordering::Relaxed);
    if process::is_running(final_pid)? {
        println!("[mik] Stopping instance...");
        process::kill_instance(final_pid)?;
    }

    // Update state to stopped
    let mut stopped = store.get_instance(name)?.unwrap_or(instance);
    stopped.status = Status::Stopped;
    store.save_instance(&stopped)?;

    Ok(())
}

/// Handle a single watch event by restarting the instance if needed.
fn handle_watch_event(
    event: crate::daemon::watch::WatchEvent,
    pid: &std::sync::Arc<std::sync::atomic::AtomicU32>,
    name: &str,
    spawn_config: &SpawnConfig,
) {
    use crate::daemon::watch::WatchEvent;

    match event {
        WatchEvent::ModuleChanged { .. } | WatchEvent::ConfigChanged => {
            let old_pid = pid.load(std::sync::atomic::Ordering::Relaxed);

            // Kill old process
            if process::is_running(old_pid).unwrap_or(false)
                && let Err(e) = process::kill_instance(old_pid)
            {
                eprintln!("[mik] Failed to stop old instance: {e}");
                return;
            }

            // Spawn new process
            let start = std::time::Instant::now();
            match process::spawn_instance(spawn_config) {
                Ok(new_info) => {
                    pid.store(new_info.pid, std::sync::atomic::Ordering::Relaxed);
                    println!(
                        "[mik] Reloaded {} (took {}ms)",
                        name,
                        start.elapsed().as_millis()
                    );

                    // Update state
                    update_instance_state_after_reload(name, spawn_config, new_info.pid);
                },
                Err(e) => {
                    eprintln!("[mik] Failed to restart: {e}");
                },
            }
        },
        WatchEvent::ModuleRemoved { .. } => {
            // Just log, don't restart
        },
        WatchEvent::Error { message } => {
            eprintln!("[mik] Watch error: {message}");
        },
    }
}

/// Update instance state in the store after a hot reload.
fn update_instance_state_after_reload(name: &str, spawn_config: &SpawnConfig, new_pid: u32) {
    if let Some(store) = get_state_path().ok().and_then(|p| StateStore::open(p).ok()) {
        let updated = Instance {
            name: name.to_string(),
            port: spawn_config.port,
            pid: new_pid,
            status: Status::Running,
            config: spawn_config.config_path.clone(),
            started_at: Utc::now(),
            modules: vec![],
            auto_restart: false,
            restart_count: 0,
            last_restart_at: None,
        };
        let _ = store.save_instance(&updated);
    }
}

/// Stop a running WASM instance.
///
/// Gracefully terminates the process and updates state.
pub fn down(name: &str) -> Result<()> {
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

    Ok(())
}

/// List all tracked WASM instances.
///
/// Shows status, port, PID, and uptime for each instance.
pub fn ps() -> Result<()> {
    let state_path = get_state_path()?;

    // Check if state file exists
    if !state_path.exists() {
        println!("No instances found. Start one with 'mik up'");
        return Ok(());
    }

    let store = StateStore::open(&state_path)?;
    let instances = store.list_instances()?;

    if instances.is_empty() {
        println!("No instances found. Start one with 'mik up'");
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
        println!("No instances found. Start one with 'mik up'");
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
                let (cpu, mem, status) =
                    if let Some(proc) = system.process(Pid::from(instance.pid as usize)) {
                        let cpu = format!("{:.1}%", proc.cpu_usage());
                        let mem = format_bytes(proc.memory());
                        (cpu, mem, "running".to_string())
                    } else {
                        ("-".to_string(), "-".to_string(), "stopped".to_string())
                    };

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
