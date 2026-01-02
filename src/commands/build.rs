//! Build WASI component from source.
//!
//! Supports multiple languages: Rust (default) and `TypeScript`.
//! Uses cargo-component or jco depending on language.
//! Optionally composes all dependencies using wac.
//! Outputs packaged component to dist/ folder.

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{check_tool, require_tool, require_tool_with_info};
use crate::manifest::{Dependency, Manifest};
use crate::ui;
use crate::utils::{format_bytes, get_cargo_name};

/// Normalize language string to canonical form.
fn normalize_language(lang: &str) -> &'static str {
    match lang.to_lowercase().as_str() {
        "rust" | "rs" => "rust",
        "typescript" | "ts" => "typescript",
        _ => "rust", // default
    }
}

/// Detect the native host target triple for running tests.
///
/// Returns the appropriate target triple for the current platform:
/// - Windows: `x86_64-pc-windows-msvc` or `x86_64-pc-windows-gnu`
/// - macOS x86: `x86_64-apple-darwin`
/// - macOS ARM: `aarch64-apple-darwin`
/// - Linux x86: `x86_64-unknown-linux-gnu`
/// - Linux ARM: `aarch64-unknown-linux-gnu`
fn detect_host_target() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64", target_env = "gnu"))]
    {
        "x86_64-pc-windows-gnu"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    // Fallback for other platforms
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    {
        // Try to get from rustc if we're on an unsupported platform
        "x86_64-unknown-linux-gnu"
    }
}

/// Extract OpenAPI schema from handler by running the mik-sdk schema test.
///
/// The mik-sdk generates schemas at build time via `cargo test __mik_write_schema`.
/// This creates an `openapi.json` file in the handler directory.
///
/// # Returns
/// - `Ok(Some(path))` if schema was extracted successfully
/// - `Ok(None)` if the handler doesn't use routes! macro (no schema test)
/// - `Err` only for unexpected failures
fn extract_schema() -> Result<Option<PathBuf>> {
    // Check for Cargo.toml (Rust project)
    if !Path::new("Cargo.toml").exists() {
        return Ok(None);
    }

    let host_target = detect_host_target();

    println!();
    println!("Extracting OpenAPI schema...");
    println!("  Target: {host_target}");

    let spinner = ui::create_spinner("Running schema extraction test...");

    // Run: cargo test __mik_write_schema --target <host> -- --exact --nocapture
    let output = Command::new("cargo")
        .args([
            "test",
            "__mik_write_schema",
            "--target",
            host_target,
            "--",
            "--exact",
            "--nocapture",
        ])
        .output()
        .context("Failed to run cargo test for schema extraction")?;

    spinner.finish_and_clear();

    if output.status.success() {
        // Check if openapi.json was created
        let schema_path = PathBuf::from("openapi.json");
        if schema_path.exists() {
            let size = fs::metadata(&schema_path).map(|m| m.len()).unwrap_or(0);
            println!(
                "Schema extracted: {} ({})",
                schema_path.display(),
                format_bytes(size)
            );
            return Ok(Some(schema_path));
        }
        // Test passed but no schema file - handler might not export routes
        println!("  Note: Schema test passed but no openapi.json generated");
        return Ok(None);
    }

    // Check if the test simply doesn't exist (handler doesn't use routes! macro)
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Common patterns when test doesn't exist
    if stderr.contains("no tests to run")
        || stderr.contains("0 passed")
        || stdout.contains("0 passed; 0 failed")
        || stderr.contains("could not compile")
        || stderr.contains("no test named")
    {
        println!(
            "  Note: Handler does not export routes! macro schema (test not found or compilation failed)"
        );
        return Ok(None);
    }

    // Test exists but failed - this is a warning, not a build failure
    eprintln!("  Warning: Schema extraction test failed");
    if !stderr.is_empty() {
        // Only show first few lines of error
        let error_preview: String = stderr.lines().take(5).collect::<Vec<_>>().join("\n");
        eprintln!("  {error_preview}");
    }

    Ok(None)
}

/// Build the component.
///
/// # Arguments
/// - `release`: Build in release mode with optimizations
/// - `compose`: Compose with dependencies using wac
/// - `lang_override`: Override detected language (rust/typescript)
/// - `no_schema`: Skip OpenAPI schema extraction
pub async fn execute(
    release: bool,
    compose: bool,
    lang_override: Option<String>,
    no_schema: bool,
) -> Result<()> {
    // Load mik.toml if it exists
    let manifest = Manifest::load().ok();

    // Resolve language: flag > mik.toml > rust
    let language = lang_override
        .as_deref()
        .or_else(|| {
            manifest
                .as_ref()
                .and_then(|m| m.project.language.as_deref())
        })
        .map(normalize_language)
        .unwrap_or("rust");

    // Get project name
    let name = manifest
        .as_ref()
        .map(|m| m.project.name.clone())
        .or_else(get_cargo_name)
        .unwrap_or_else(|| "component".to_string());

    println!("Building: {name} ({language})");

    // Step 1: Extract schema BEFORE building WASM (for Rust projects only)
    // This runs cargo test which needs the native target, not wasm32-wasip2
    let schema_path = if !no_schema && language == "rust" {
        extract_schema()?
    } else {
        None
    };

    // Step 2: Build based on language
    let (wasm_path, target_base) = match language {
        "rust" => build_rust(&name, release)?,
        "typescript" => build_typescript(&name)?,
        _ => bail!("Unsupported language: {language}"),
    };

    // Optimize WASM (strip debug info, names, etc.) in release mode
    if release && language == "rust" {
        optimize_wasm(&wasm_path)?;
    }

    // Compose HTTP handler with bridge (if enabled)
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

    // Step 4: Package to dist/ folder (including schema if present)
    package_to_dist(
        &final_wasm,
        &name,
        release,
        did_compose,
        schema_path.as_deref(),
    )?;

    Ok(())
}

// =============================================================================
// Language-specific build functions
// =============================================================================

/// Build Rust project with cargo-component.
fn build_rust(name: &str, release: bool) -> Result<(PathBuf, PathBuf)> {
    // Check for Cargo.toml
    if !Path::new("Cargo.toml").exists() {
        bail!("No Cargo.toml found. Run from a Rust project directory or use --lang flag.");
    }

    // Check for cargo-component
    require_tool("cargo-component", "cargo install cargo-component")?;

    // Build with cargo-component
    let mut args = vec!["component", "build", "--target", "wasm32-wasip2"];
    if release {
        args.push("--release");
        println!("Mode: release");
    } else {
        println!("Mode: debug");
    }

    let spinner = ui::create_spinner("Building component...");

    let output = Command::new("cargo")
        .args(&args)
        .output()
        .context("Failed to run cargo component build")?;

    spinner.finish_and_clear();

    if !output.status.success() {
        ui::print_error_box_from_output("Rust Build Failed", &output);
        bail!("Rust compilation failed");
    }

    let build_type = if release { "release" } else { "debug" };
    let crate_name = name.replace('-', "_");
    let target_base = find_target_dir()?;

    let wasm_path = target_base
        .join("wasm32-wasip2")
        .join(build_type)
        .join(format!("{crate_name}.wasm"));

    if !wasm_path.exists() {
        bail!("WASM output not found: {}", wasm_path.display());
    }

    Ok((wasm_path, target_base))
}

/// Get the npm command (npm.cmd on Windows, npm elsewhere).
fn npm_command() -> Command {
    #[cfg(windows)]
    {
        Command::new("cmd")
    }
    #[cfg(not(windows))]
    {
        Command::new("npm")
    }
}

/// Get npm command arguments (with /c npm prefix on Windows).
#[cfg(windows)]
fn npm_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut result: Vec<&'a str> = vec!["/c", "npm"];
    result.extend(args.iter().copied());
    result
}

#[cfg(not(windows))]
fn npm_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.to_vec()
}

/// Check if npm is available.
fn check_npm() -> bool {
    npm_command()
        .args(npm_args(&["--version"]))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build `TypeScript` project with jco componentize.
fn build_typescript(name: &str) -> Result<(PathBuf, PathBuf)> {
    // Check for package.json
    if !Path::new("package.json").exists() {
        bail!("No package.json found. Run from a TypeScript project directory.");
    }

    // Check for npm
    if !check_npm() {
        bail!("npm not found. Install Node.js to build TypeScript projects.");
    }

    // Fetch WIT dependencies if wit/deps doesn't exist but deps.toml does
    if Path::new("wit/deps.toml").exists() && !Path::new("wit/deps").exists() {
        println!("Fetching WIT dependencies...");
        let output = Command::new("wkg")
            .args(["wit", "fetch"])
            .output()
            .context("Failed to run wkg wit fetch. Install wkg: cargo install wkg")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("Warning: wkg wit fetch failed: {stderr}");
            eprintln!("You may need to install wkg: cargo install wkg");
        }
    }

    // Install dependencies if node_modules doesn't exist
    if !Path::new("node_modules").exists() {
        println!("Installing npm dependencies...");
        let output = npm_command()
            .args(npm_args(&["install"]))
            .output()
            .context("Failed to run npm install")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("npm install failed: {stderr}");
        }
    }

    // Run npm build
    let spinner = ui::create_spinner("Building TypeScript component...");

    let output = npm_command()
        .args(npm_args(&["run", "build"]))
        .output()
        .context("Failed to run npm run build")?;

    spinner.finish_and_clear();

    if !output.status.success() {
        ui::print_error_box_from_output("TypeScript Build Failed", &output);
        bail!("TypeScript build failed");
    }

    // Find the output wasm file
    let wasm_path = PathBuf::from(format!("{name}.wasm"));
    if !wasm_path.exists() {
        // Try handler.wasm as fallback
        let fallback = PathBuf::from("handler.wasm");
        if fallback.exists() {
            return Ok((fallback, PathBuf::from(".")));
        }
        bail!("WASM output not found: {name}.wasm or handler.wasm");
    }

    Ok((wasm_path, PathBuf::from(".")))
}

// =============================================================================
// Helper functions
// =============================================================================

/// Package the built component to dist/ folder with tar.gz.
///
/// If `schema_path` is provided, the OpenAPI schema is copied to `dist/<name>.openapi.json`
/// and included in the tar.gz package.
fn package_to_dist(
    wasm_path: &Path,
    name: &str,
    release: bool,
    composed: bool,
    schema_path: Option<&Path>,
) -> Result<()> {
    // Create dist directory
    let dist_dir = Path::new("dist");
    fs::create_dir_all(dist_dir)?;

    // Determine output filename
    let suffix = if composed { "-composed" } else { "" };
    let mode = if release { "release" } else { "debug" };
    let wasm_name = format!("{name}{suffix}.wasm");
    let tar_name = format!("{name}{suffix}-{mode}.tar.gz");
    let schema_name = format!("{name}.openapi.json");

    let dist_wasm = dist_dir.join(&wasm_name);
    let dist_tar = dist_dir.join(&tar_name);
    let dist_schema = dist_dir.join(&schema_name);

    // Copy wasm to dist/
    fs::copy(wasm_path, &dist_wasm).context("Failed to copy wasm to dist/")?;

    // Get wasm size
    let wasm_size = fs::metadata(&dist_wasm).map(|m| m.len()).unwrap_or(0);

    // Copy schema to dist/ if present
    let schema_size = if let Some(schema) = schema_path {
        fs::copy(schema, &dist_schema).context("Failed to copy schema to dist/")?;
        fs::metadata(&dist_schema).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Create tar.gz
    {
        let tar_file = File::create(&dist_tar).context("Failed to create tar.gz")?;
        let encoder = GzEncoder::new(tar_file, Compression::best());
        let mut tar = tar::Builder::new(encoder);

        // Add wasm to tar
        let mut wasm_file = File::open(&dist_wasm)?;
        tar.append_file(&wasm_name, &mut wasm_file)?;

        // Add schema to tar if present
        if schema_path.is_some() && dist_schema.exists() {
            let mut schema_file = File::open(&dist_schema)?;
            tar.append_file(&schema_name, &mut schema_file)?;
        }

        // Finish tar and properly close the encoder
        let encoder = tar.into_inner()?;
        encoder.finish()?;
    }

    // Get tar.gz size (after file is fully written)
    let tar_size = fs::metadata(&dist_tar).map(|m| m.len()).unwrap_or(0);

    // Print summary
    ui::print_summary_header("Build Summary");
    println!("Output:     dist/{wasm_name}");
    if schema_path.is_some() {
        println!("Schema:     dist/{schema_name}");
    }
    println!("Package:    dist/{tar_name}");
    println!();
    println!("WASM size:  {}", format_bytes(wasm_size));
    if schema_size > 0 {
        println!("Schema:     {}", format_bytes(schema_size));
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = (tar_size as f64 / wasm_size as f64) * 100.0;
    println!(
        "Compressed: {} ({:.1}% of original)",
        format_bytes(tar_size),
        ratio
    );
    ui::print_summary_footer();

    Ok(())
}

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

    let spinner = ui::create_spinner("Composing with bridge...");

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
        ui::print_error_box_from_output("Bridge composition failed", &output);
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
    if let Ok(tools_dir) = crate::daemon::paths::get_tools_dir() {
        let tools_path = tools_dir.join("bridge/latest.wasm");
        if tools_path.exists() {
            return Ok(tools_path);
        }
    }

    // 4. Auto-download from OCI registry (only when registry feature is enabled)
    #[cfg(feature = "registry")]
    {
        println!("  Bridge not found locally, downloading from registry...");
        let bridge_path = download_bridge().await?;
        Ok(bridge_path)
    }

    #[cfg(not(feature = "registry"))]
    anyhow::bail!(
        "Bridge component not found and registry feature is disabled.\n\n\
         Options:\n\
         1. Place bridge.wasm in modules/bridge.wasm\n\
         2. Add to mik.toml: [composition]\\n   bridge = \"path/to/bridge.wasm\"\n\
         3. Rebuild mik with registry feature enabled"
    )
}

/// Download the bridge component from OCI registry.
///
/// Downloads to `~/.mik/tools/bridge/latest.wasm`.
#[cfg(feature = "registry")]
async fn download_bridge() -> Result<PathBuf> {
    let tools_dir = crate::daemon::paths::get_tools_dir()?.join("bridge");
    let output_path = tools_dir.join("latest.wasm");

    // Create directory if needed
    fs::create_dir_all(&tools_dir).context("Failed to create ~/.mik/tools/bridge directory")?;

    let spinner = ui::create_spinner("Downloading bridge from registry...");

    // Use pull_oci from pull module
    match super::pull::pull_oci(BRIDGE_OCI_REF, &output_path).await {
        Ok(()) => {
            spinner.finish_and_clear();
            println!("  Downloaded bridge to {}", output_path.display());
            Ok(output_path)
        },
        Err(e) => {
            spinner.finish_and_clear();
            ui::print_error_section(
                "Failed to download bridge from registry",
                &format!(
                    "Error: {e}\n\nThe bridge component could not be downloaded from:\n  {BRIDGE_OCI_REF}"
                ),
            );
            ui::print_numbered_steps(&[
                "Check your network connection",
                "Specify a local path in mik.toml:\n     [composition]\n     bridge = \"path/to/bridge.wasm\"",
                "Place bridge.wasm in modules/ directory",
                "Build from mik-sdk:\n     cd mik-sdk/mik-bridge && cargo component build --release",
            ]);
            anyhow::bail!("Failed to download bridge: {e}")
        },
    }
}

/// Check that wac tool is available, printing helpful error if not.
fn check_wac_available() -> Result<()> {
    require_tool_with_info(
        "wac",
        "cargo install wac-cli",
        Some("https://github.com/bytecodealliance/wac"),
    )
}

/// Collect dependency paths from manifest, returning found paths.
fn collect_dependency_paths(manifest: &Manifest) -> Vec<String> {
    let spinner = ui::create_spinner("Collecting dependencies...");

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
    dep_paths
}

/// Build wac plug command arguments.
fn build_wac_args(main_component: &Path, output_path: &Path, dep_paths: &[String]) -> Vec<String> {
    let mut wac_args: Vec<String> = vec!["plug".to_string()];

    for path in dep_paths {
        wac_args.push("--plug".to_string());
        wac_args.push(path.clone());
    }

    wac_args.push(main_component.to_string_lossy().to_string());
    wac_args.push("-o".to_string());
    wac_args.push(output_path.to_string_lossy().to_string());

    wac_args
}

/// Run wac composition command with given arguments.
fn run_wac_compose(wac_args: &[String]) -> Result<std::process::Output> {
    println!();
    println!("Running: wac {}", wac_args.join(" "));

    let spinner = ui::create_spinner("Composing components...");

    let output = Command::new("wac")
        .args(wac_args)
        .output()
        .context("Failed to run wac")?;

    spinner.finish_and_clear();
    Ok(output)
}

/// Print detailed composition error with troubleshooting hints.
fn print_composition_error(output: &std::process::Output) {
    ui::print_error_box_with_hints(
        "Composition Failed",
        output,
        &[
            "Incompatible WIT interfaces:\n   Ensure all components export/import matching interfaces",
            "Missing dependency components:\n   Run 'mik pull' to download dependencies from registry",
            "Invalid component format:\n   Verify all .wasm files are valid WASI components:\n   wasm-tools validate component.wasm",
            "Dependency version mismatch:\n   Check mik.toml dependency versions match built components\n\nFor debugging, inspect components with:\n  wasm-tools component wit <component.wasm>",
        ],
    );
}

/// Compose the main component with all dependencies from mik.toml.
/// Returns the path to composed.wasm if composition was successful.
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

    check_wac_available()?;

    let dep_paths = collect_dependency_paths(manifest);

    if dep_paths.is_empty() {
        println!("No dependency components found in modules/");
        println!("Run 'mik add' to install dependencies first");
        return Ok(None);
    }

    let output_path = target_base.join("composed.wasm");
    fs::create_dir_all(target_base)?;

    let wac_args = build_wac_args(main_component, &output_path, &dep_paths);
    let output = run_wac_compose(&wac_args)?;

    if !output.status.success() {
        print_composition_error(&output);
        anyhow::bail!("Component composition failed");
    }

    println!();

    optimize_wasm(&output_path)?;

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
        Dependency::Detailed(d) => d.path.as_ref().map_or_else(
            || format!("modules/{name}.wasm"),
            |path| {
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
            },
        ),
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

    let spinner = ui::create_spinner("Stripping component (removing debug info)...");

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

    let spinner = ui::create_spinner("Optimizing WASM for size...");

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
