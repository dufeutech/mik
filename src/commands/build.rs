//! Build WASI component from Rust project.
//!
//! Uses cargo-component to build, optionally composes all dependencies using wac.
//! Outputs packaged component to dist/ folder.

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::check_tool;
use crate::manifest::{Dependency, Manifest};
use crate::utils::{format_bytes, get_cargo_name};

/// Progress spinner tick interval in milliseconds.
/// Controls how frequently the spinner animation updates (100ms = 10 FPS).
const SPINNER_TICK_INTERVAL_MS: u64 = 100;

/// Build the component.
#[allow(clippy::too_many_lines)]
pub async fn execute(release: bool, compose: bool) -> Result<()> {
    // Check for Cargo.toml (Rust project)
    if !Path::new("Cargo.toml").exists() {
        anyhow::bail!("No Cargo.toml found. Run from a Rust project directory.");
    }

    // Load mik.toml if it exists
    let manifest = Manifest::load().ok();
    let name = manifest
        .as_ref()
        .map(|m| m.project.name.clone())
        .or_else(get_cargo_name)
        .unwrap_or_else(|| "component".to_string());

    println!("Building: {name}");

    // Check for cargo-component
    if check_tool("cargo-component").is_err() {
        eprintln!("\nError: cargo-component not found\n");
        eprintln!("cargo-component is required to build WASI components.");
        eprintln!("\nInstall with:");
        eprintln!("  cargo install cargo-component");
        eprintln!("\nFor more information, visit:");
        eprintln!("  https://github.com/bytecodealliance/cargo-component");
        anyhow::bail!("Missing required tool: cargo-component");
    }

    // Build with cargo-component
    let mut args = vec!["component", "build", "--target", "wasm32-wasip2"];
    if release {
        args.push("--release");
        println!("Mode: release");
    } else {
        println!("Mode: debug");
    }

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Building component...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(SPINNER_TICK_INTERVAL_MS));

    let output = Command::new("cargo")
        .args(&args)
        .output()
        .context("Failed to run cargo component build")?;

    spinner.finish_and_clear();

    if !output.status.success() {
        eprintln!("\n{}", "=".repeat(60));
        eprintln!("Build Failed");
        eprintln!("{}", "=".repeat(60));

        // Print stderr if available
        if !output.stderr.is_empty()
            && let Ok(stderr) = String::from_utf8(output.stderr.clone())
        {
            eprintln!("\n{stderr}");
        }

        // Print stdout if available (cargo often puts errors here too)
        if !output.stdout.is_empty()
            && let Ok(stdout) = String::from_utf8(output.stdout.clone())
        {
            eprintln!("{stdout}");
        }

        eprintln!("\n{}", "=".repeat(60));
        eprintln!("Common Issues:");
        eprintln!("{}", "=".repeat(60));
        eprintln!("\n1. Missing wasi-http dependency:");
        eprintln!("   Add to Cargo.toml:");
        eprintln!("   [dependencies]");
        eprintln!("   wasi-http = \"0.2\"");
        eprintln!("\n2. Wrong target configuration:");
        eprintln!("   Ensure Cargo.toml has:");
        eprintln!("   [package.metadata.component.target]");
        eprintln!("   path = \"wit\"");
        eprintln!("\n3. Missing WIT files:");
        eprintln!("   Create wit/ directory with world definition");
        eprintln!("\n4. Incompatible dependencies:");
        eprintln!("   Ensure all deps support wasm32-wasip2 target");
        eprintln!("\nFor more help, check your project's wit/ directory and Cargo.toml\n");

        anyhow::bail!("Compilation failed");
    }

    let build_type = if release { "release" } else { "debug" };

    // Rust converts dashes to underscores in crate names
    let crate_name = name.replace('-', "_");

    // Find the target directory (may be in workspace root)
    let target_base = find_target_dir()?;

    let wasm_path = target_base
        .join("wasm32-wasip2")
        .join(build_type)
        .join(format!("{crate_name}.wasm"));

    // Verify the WASM file was created
    if !wasm_path.exists() {
        eprintln!("\nError: WASM file not found at expected location");
        eprintln!("Expected: {}", wasm_path.display());
        eprintln!("\nPossible causes:");
        eprintln!("1. Build succeeded but output location is different");
        eprintln!("2. Crate name mismatch (Cargo.toml vs mik.toml)");
        eprintln!("3. Workspace configuration issue");
        eprintln!("\nTry:");
        eprintln!("  - Check crate name in Cargo.toml matches mik.toml");
        eprintln!("  - Look for .wasm files: find target -name '*.wasm'");
        anyhow::bail!("WASM output file not found");
    }

    // Optimize with wasm-opt for release builds
    if release {
        optimize_wasm(&wasm_path)?;
    }

    // Compose HTTP handler with bridge (if enabled and app type)
    let http_composed = if let Some(ref m) = manifest {
        compose_http_handler(&wasm_path, &target_base, m).await?
    } else {
        None
    };

    // Use HTTP-composed output if available, otherwise raw handler
    let handler_wasm = http_composed.as_ref().unwrap_or(&wasm_path);

    // Compose all dependencies if requested
    let final_wasm = if compose {
        if let Some(ref m) = manifest {
            let composed = compose_all(handler_wasm, &target_base, m)?;
            composed.unwrap_or_else(|| handler_wasm.clone())
        } else {
            println!("No mik.toml found, skipping composition");
            handler_wasm.clone()
        }
    } else {
        handler_wasm.clone()
    };

    // Determine if we did any composition
    let did_compose = compose || http_composed.is_some();

    // Package to dist/ folder
    package_to_dist(&final_wasm, &name, release, did_compose)?;

    Ok(())
}

/// Package the built component to dist/ folder with tar.gz.
fn package_to_dist(wasm_path: &Path, name: &str, release: bool, composed: bool) -> Result<()> {
    // Create dist directory
    let dist_dir = Path::new("dist");
    fs::create_dir_all(dist_dir)?;

    // Determine output filename
    let suffix = if composed { "-composed" } else { "" };
    let mode = if release { "release" } else { "debug" };
    let wasm_name = format!("{name}{suffix}.wasm");
    let tar_name = format!("{name}{suffix}-{mode}.tar.gz");

    let dist_wasm = dist_dir.join(&wasm_name);
    let dist_tar = dist_dir.join(&tar_name);

    // Copy wasm to dist/
    fs::copy(wasm_path, &dist_wasm).context("Failed to copy wasm to dist/")?;

    // Get wasm size
    let wasm_size = fs::metadata(&dist_wasm).map(|m| m.len()).unwrap_or(0);

    // Create tar.gz
    {
        let tar_file = File::create(&dist_tar).context("Failed to create tar.gz")?;
        let encoder = GzEncoder::new(tar_file, Compression::best());
        let mut tar = tar::Builder::new(encoder);

        // Add wasm to tar
        let mut wasm_file = File::open(&dist_wasm)?;
        tar.append_file(&wasm_name, &mut wasm_file)?;

        // Finish tar and properly close the encoder
        let encoder = tar.into_inner()?;
        encoder.finish()?;
    }

    // Get tar.gz size (after file is fully written)
    let tar_size = fs::metadata(&dist_tar).map(|m| m.len()).unwrap_or(0);

    // Print summary
    println!();
    println!("{}", "=".repeat(50));
    println!("Build Summary");
    println!("{}", "=".repeat(50));
    println!();
    println!("Output:     dist/{wasm_name}");
    println!("Package:    dist/{tar_name}");
    println!();
    println!("WASM size:  {}", format_bytes(wasm_size));
    #[allow(clippy::cast_precision_loss)]
    let ratio = (tar_size as f64 / wasm_size as f64) * 100.0;
    println!(
        "Compressed: {} ({:.1}% of original)",
        format_bytes(tar_size),
        ratio
    );
    println!();
    println!("{}", "=".repeat(50));

    Ok(())
}

/// Default location for tools (bridge, etc) in user's home directory.
const DEFAULT_TOOLS_DIR: &str = ".mik/tools";
/// OCI reference for the bridge component.
const BRIDGE_OCI_REF: &str = "ghcr.io/dufeut/mik-sdk-bridge";

/// Compose a mik handler with bridge to create a WASI HTTP component.
///
/// The mik SDK uses a two-component composition pattern:
/// 1. Handler exports `mik:core/handler@0.2.0`
/// 2. Bridge translates to `wasi:http/incoming-handler@0.2.0`
///
/// Composition: `wac plug bridge --plug handler -o service.wasm`
async fn compose_http_handler(
    handler: &Path,
    target_base: &Path,
    manifest: &Manifest,
) -> Result<Option<PathBuf>> {
    // Check if HTTP handler composition is enabled
    if !manifest.composition.http_handler {
        return Ok(None);
    }

    // Only compose "app" type projects (handlers)
    if manifest.project.r#type != "app" {
        return Ok(None);
    }

    println!();
    println!("Composing HTTP handler with bridge...");

    // Check wac is available
    if check_tool("wac").is_err() {
        eprintln!("\nError: wac not found\n");
        eprintln!("wac is required for HTTP handler composition.");
        eprintln!("\nInstall with:");
        eprintln!("  cargo install wac-cli");
        anyhow::bail!("Missing required tool: wac");
    }

    // Find bridge component (with auto-download from OCI if needed)
    let bridge = find_bridge(manifest.composition.bridge.as_deref()).await?;

    println!("  Bridge: {}", bridge.display());

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Composing with bridge...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(SPINNER_TICK_INTERVAL_MS));

    // Compose bridge with handler
    // wac plug bridge.wasm --plug handler.wasm -o service.wasm
    let service = target_base.join("service.wasm");
    let output = Command::new("wac")
        .args([
            "plug",
            &bridge.to_string_lossy(),
            "--plug",
            &handler.to_string_lossy(),
            "-o",
            &service.to_string_lossy(),
        ])
        .output()
        .context("Failed to run wac for bridge composition")?;

    spinner.finish_and_clear();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("\nBridge composition failed:");
        eprintln!("{stderr}");
        eprintln!("\nThis usually means the handler doesn't export mik:core/handler@0.2.0");
        eprintln!(
            "Verify with: wasm-tools component wit {}",
            handler.display()
        );
        anyhow::bail!("Bridge composition failed");
    }

    // Optimize the composed output
    optimize_wasm(&service)?;

    let size = fs::metadata(&service).map(|m| m.len()).unwrap_or(0);
    println!(
        "HTTP handler composed: {} ({})",
        service.display(),
        format_bytes(size)
    );

    Ok(Some(service))
}

/// Find the bridge component, with auto-download from OCI registry if not found.
///
/// Discovery order:
/// 1. Explicit path in `[composition]` config
/// 2. Local `modules/bridge.wasm`
/// 3. `~/.mik/tools/bridge/latest.wasm`
/// 4. Auto-download from OCI registry
async fn find_bridge(configured_path: Option<&str>) -> Result<PathBuf> {
    // 1. Check configured path
    if let Some(path) = configured_path {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!(
            "Configured bridge path does not exist: {path}\n\
             Fix: Update [composition].bridge in mik.toml"
        );
    }

    // 2. Check local modules/ directory
    let local = PathBuf::from("modules/bridge.wasm");
    if local.exists() {
        return Ok(local);
    }

    // 3. Check ~/.mik/tools/bridge/latest.wasm
    if let Some(home) = dirs::home_dir() {
        let tools_path = home.join(DEFAULT_TOOLS_DIR).join("bridge/latest.wasm");
        if tools_path.exists() {
            return Ok(tools_path);
        }
    }

    // 4. Auto-download from OCI registry
    println!("  Bridge not found locally, downloading from registry...");
    let bridge_path = download_bridge().await?;
    Ok(bridge_path)
}

/// Download the bridge component from OCI registry.
///
/// Downloads to `~/.mik/tools/bridge/latest.wasm`.
async fn download_bridge() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let tools_dir = home.join(DEFAULT_TOOLS_DIR).join("bridge");
    let output_path = tools_dir.join("latest.wasm");

    // Create directory if needed
    fs::create_dir_all(&tools_dir).context("Failed to create ~/.mik/tools/bridge directory")?;

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Downloading bridge from registry...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(SPINNER_TICK_INTERVAL_MS));

    // Use pull_oci from pull module
    match super::pull::pull_oci(BRIDGE_OCI_REF, &output_path).await {
        Ok(()) => {
            spinner.finish_and_clear();
            println!("  Downloaded bridge to {}", output_path.display());
            Ok(output_path)
        },
        Err(e) => {
            spinner.finish_and_clear();
            eprintln!("\n{}", "=".repeat(60));
            eprintln!("Failed to download bridge from registry");
            eprintln!("{}", "=".repeat(60));
            eprintln!();
            eprintln!("Error: {e}");
            eprintln!();
            eprintln!("The bridge component could not be downloaded from:");
            eprintln!("  {BRIDGE_OCI_REF}");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  1. Check your network connection");
            eprintln!("  2. Specify a local path in mik.toml:");
            eprintln!("     [composition]");
            eprintln!("     bridge = \"path/to/bridge.wasm\"");
            eprintln!();
            eprintln!("  3. Place bridge.wasm in modules/ directory");
            eprintln!();
            eprintln!("  4. Build from mik-sdk:");
            eprintln!("     cd mik-sdk/mik-bridge && cargo component build --release");
            eprintln!("{}", "=".repeat(60));
            anyhow::bail!("Failed to download bridge: {e}")
        },
    }
}

/// Compose the main component with all dependencies from mik.toml.
/// Returns the path to composed.wasm if composition was successful.
#[allow(clippy::too_many_lines)]
fn compose_all(
    main_component: &Path,
    target_base: &Path,
    manifest: &Manifest,
) -> Result<Option<PathBuf>> {
    if manifest.dependencies.is_empty() {
        println!("No dependencies to compose");
        return Ok(None);
    }

    println!();
    println!(
        "Composing with {} dependencies...",
        manifest.dependencies.len()
    );

    // Check wac is available
    if check_tool("wac").is_err() {
        eprintln!("\nError: wac not found\n");
        eprintln!("wac is required for component composition.");
        eprintln!("\nInstall with:");
        eprintln!("  cargo install wac-cli");
        eprintln!("\nFor more information, visit:");
        eprintln!("  https://github.com/bytecodealliance/wac");
        anyhow::bail!("Missing required tool: wac");
    }

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Collecting dependencies...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(SPINNER_TICK_INTERVAL_MS));

    // Collect all dependency paths
    let mut dep_paths: Vec<String> = Vec::new();

    for (name, dep) in &manifest.dependencies {
        let path = resolve_dependency_path(name, dep);
        if Path::new(&path).exists() {
            spinner.set_message(format!("Found {name}"));
            println!("  + {name}: {path}");
            dep_paths.push(path);
        } else {
            println!("  ! {name}: not found at {path}");
        }
    }

    spinner.finish_and_clear();

    if dep_paths.is_empty() {
        println!("No dependency components found in modules/");
        println!("Run 'mik add' to install dependencies first");
        return Ok(None);
    }

    // Create output directory
    let output_path = target_base.join("composed.wasm");
    fs::create_dir_all(target_base)?;

    // Compose: start with main, plug each dependency
    // wac plug --plug dep1.wasm --plug dep2.wasm main.wasm -o composed.wasm
    let mut wac_args: Vec<String> = vec!["plug".to_string()];

    for path in &dep_paths {
        wac_args.push("--plug".to_string());
        wac_args.push(path.clone());
    }

    let main_str = main_component.to_string_lossy().to_string();
    let output_str = output_path.to_string_lossy().to_string();

    wac_args.push(main_str);
    wac_args.push("-o".to_string());
    wac_args.push(output_str.clone());

    println!();
    println!("Running: wac {}", wac_args.join(" "));

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Composing components...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(SPINNER_TICK_INTERVAL_MS));

    let output = Command::new("wac")
        .args(&wac_args)
        .output()
        .context("Failed to run wac")?;

    spinner.finish_and_clear();

    if !output.status.success() {
        eprintln!("\n{}", "=".repeat(60));
        eprintln!("Composition Failed");
        eprintln!("{}", "=".repeat(60));

        // Print stderr if available
        if !output.stderr.is_empty()
            && let Ok(stderr) = String::from_utf8(output.stderr.clone())
        {
            eprintln!("\n{stderr}");
        }

        // Print stdout if available
        if !output.stdout.is_empty()
            && let Ok(stdout) = String::from_utf8(output.stdout.clone())
        {
            eprintln!("{stdout}");
        }

        eprintln!("\n{}", "=".repeat(60));
        eprintln!("Common Issues:");
        eprintln!("{}", "=".repeat(60));
        eprintln!("\n1. Incompatible WIT interfaces:");
        eprintln!("   Ensure all components export/import matching interfaces");
        eprintln!("\n2. Missing dependency components:");
        eprintln!("   Run 'mik pull' to download dependencies from registry");
        eprintln!("\n3. Invalid component format:");
        eprintln!("   Verify all .wasm files are valid WASI components:");
        eprintln!("   wasm-tools validate component.wasm");
        eprintln!("\n4. Dependency version mismatch:");
        eprintln!("   Check mik.toml dependency versions match built components");
        eprintln!("\nFor debugging, inspect components with:");
        eprintln!("  wasm-tools component wit <component.wasm>\n");

        anyhow::bail!("Component composition failed");
    }

    println!();

    // Optimize composed output
    optimize_wasm(&output_path)?;

    // Show composed size
    let composed_size = fs::metadata(&output_path).map(|m| m.len()).unwrap_or(0);
    println!(
        "Composed: {} ({})",
        output_path.display(),
        format_bytes(composed_size)
    );

    Ok(Some(output_path))
}

/// Resolve dependency to a local path.
fn resolve_dependency_path(name: &str, dep: &Dependency) -> String {
    match dep {
        Dependency::Simple(_) => {
            // Assume it's in modules/
            format!("modules/{name}.wasm")
        },
        Dependency::Detailed(d) => {
            if let Some(path) = &d.path {
                // Local path dependency - check extension case-insensitively
                if Path::new(path)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("wasm"))
                {
                    path.clone()
                } else {
                    // It's a directory, look for built component
                    format!("{path}/target/wasm32-wasip2/release/{name}.wasm")
                }
            } else {
                // Remote dependency - should be in modules/
                format!("modules/{name}.wasm")
            }
        },
    }
}

/// Find the target directory containing wasm32-wasip2 output.
///
/// For standalone projects, this is `./target`.
/// For workspace members, this is at the workspace root.
fn find_target_dir() -> Result<PathBuf> {
    // Look for target directory with wasm32-wasip2 subdirectory
    // This ensures we find the actual output location, not empty target dirs

    let mut current = std::env::current_dir()?;
    let original_dir = current.clone();

    // First, check local target (standalone project)
    let local_target = Path::new("target");
    if local_target.join("wasm32-wasip2").exists() {
        return Ok(local_target.to_path_buf());
    }

    // Walk up directories looking for workspace target
    loop {
        let target = current.join("target");
        if target.join("wasm32-wasip2").exists() {
            return Ok(target);
        }

        // Check if we're at a workspace root
        let cargo_toml = current.join("Cargo.toml");
        if cargo_toml.exists()
            && let Ok(content) = fs::read_to_string(&cargo_toml)
            && content.contains("[workspace]")
        {
            // This is the workspace root, target should be here after build
            return Ok(target);
        }

        // Go up one directory
        if !current.pop() {
            break;
        }
    }

    // If we couldn't find a target directory, provide helpful error
    eprintln!("\nWarning: Could not find target directory");
    eprintln!("Searched from: {}", original_dir.display());
    eprintln!("\nThe target directory will be created during build.");
    eprintln!("If this is a workspace member, ensure you're in the correct directory.");

    // Default to local target
    Ok(Path::new("target").to_path_buf())
}

/// Optimize WASM file for size.
///
/// For WASM Components: uses `wasm-tools strip --all` to remove debug info and names.
/// For Core Modules: uses `wasm-opt -Oz` for aggressive size optimization.
///
/// This can significantly reduce file size (often 50-70% for components).
fn optimize_wasm(wasm_path: &Path) -> Result<()> {
    let size_before = fs::metadata(wasm_path).map(|m| m.len()).unwrap_or(0);

    // Check if this is a WASM Component
    let is_component = is_wasm_component(wasm_path)?;

    if is_component {
        // Use wasm-tools strip for components
        optimize_component(wasm_path, size_before)?;
    } else {
        // Use wasm-opt for core modules
        optimize_core_module(wasm_path, size_before)?;
    }

    Ok(())
}

/// Optimize WASM Component using wasm-tools strip.
///
/// Removes custom sections including names, debug info, and producers.
/// Requires wasm-tools to be installed: `cargo install wasm-tools`
fn optimize_component(wasm_path: &Path, size_before: u64) -> Result<()> {
    // Check if wasm-tools is available
    if check_tool("wasm-tools").is_err() {
        // wasm-tools not installed - print helpful message
        eprintln!();
        eprintln!("Tip: Install wasm-tools for smaller components (up to 72% reduction):");
        eprintln!("  cargo install wasm-tools");
        return Ok(());
    }

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Stripping component (removing debug info)...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(SPINNER_TICK_INTERVAL_MS));

    // Run wasm-tools strip --all (removes names, debug info, custom sections)
    let output = Command::new("wasm-tools")
        .args(["strip", "--all", "-o"])
        .arg(wasm_path)
        .arg(wasm_path)
        .output()
        .context("Failed to run wasm-tools strip")?;

    spinner.finish_and_clear();

    if output.status.success() {
        let size_after = fs::metadata(wasm_path).map(|m| m.len()).unwrap_or(0);
        print_size_reduction("Stripped", size_before, size_after);
    } else {
        eprintln!("Warning: wasm-tools strip failed, using unstripped binary");
        if !output.stderr.is_empty()
            && let Ok(stderr) = String::from_utf8(output.stderr)
        {
            eprintln!("  {}", stderr.trim());
        }
    }

    Ok(())
}

/// Optimize core WASM module using wasm-opt.
fn optimize_core_module(wasm_path: &Path, size_before: u64) -> Result<()> {
    // Check if wasm-opt is available (part of binaryen)
    if check_tool("wasm-opt").is_err() {
        // wasm-opt not installed - skip silently, it's optional
        return Ok(());
    }

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Optimizing WASM for size...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(SPINNER_TICK_INTERVAL_MS));

    // Run wasm-opt -Oz (optimize for size) in-place
    let output = Command::new("wasm-opt")
        .args(["-Oz", "-o"])
        .arg(wasm_path)
        .arg(wasm_path)
        .output()
        .context("Failed to run wasm-opt")?;

    spinner.finish_and_clear();

    if output.status.success() {
        let size_after = fs::metadata(wasm_path).map(|m| m.len()).unwrap_or(0);
        print_size_reduction("Optimized", size_before, size_after);
    } else {
        eprintln!("Warning: wasm-opt optimization failed, using unoptimized binary");
        if !output.stderr.is_empty()
            && let Ok(stderr) = String::from_utf8(output.stderr)
        {
            eprintln!("  {}", stderr.trim());
        }
    }

    Ok(())
}

/// Print size reduction message.
fn print_size_reduction(action: &str, size_before: u64, size_after: u64) {
    if size_before > 0 && size_after < size_before {
        #[allow(clippy::cast_precision_loss)]
        let reduction = ((size_before - size_after) as f64 / size_before as f64) * 100.0;
        println!(
            "{action}: {} -> {} ({:.1}% smaller)",
            format_bytes(size_before),
            format_bytes(size_after),
            reduction
        );
    }
}

/// Check if a WASM file is a Component Model binary.
///
/// WASM components have magic bytes: \0asm followed by version 0x0d (13).
/// Core modules have version 0x01.
fn is_wasm_component(path: &Path) -> Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut header = [0u8; 8];
    if file.read_exact(&mut header).is_err() {
        return Ok(false);
    }
    // Check WASM magic: \0asm
    if &header[0..4] != b"\0asm" {
        return Ok(false);
    }
    // Check version byte: 0x0d = component, 0x01 = core module
    Ok(header[4] == 0x0d)
}
