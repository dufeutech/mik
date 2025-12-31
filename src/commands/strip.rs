//! Strip debug info and custom sections from WASM components.
//!
//! Wraps `wasm-tools strip` to remove debug info, names, and custom sections
//! from WASM components, significantly reducing file size.
//!
//! Auto-downloads wasm-tools if not installed.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Latest wasm-tools version to download
const WASM_TOOLS_VERSION: &str = "1.225.0";

/// Strip options
pub struct StripOptions {
    /// Remove all strippable sections (debug, names, custom)
    pub all: bool,
    /// Remove DWARF debug info
    pub debug: bool,
    /// Remove name section
    pub names: bool,
    /// Remove producers section
    pub producers: bool,
    /// Output path (default: input with .stripped.wasm suffix)
    pub output: Option<String>,
}

impl Default for StripOptions {
    fn default() -> Self {
        Self {
            all: true,
            debug: false,
            names: false,
            producers: false,
            output: None,
        }
    }
}

/// Get the path to wasm-tools, downloading if necessary
fn get_wasm_tools() -> Result<PathBuf> {
    // First check if wasm-tools is in PATH
    if let Ok(output) = Command::new("wasm-tools").arg("--version").output()
        && output.status.success()
    {
        return Ok(PathBuf::from("wasm-tools"));
    }

    // Check if we have it in ~/.mik/bin/
    let mik_bin = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
        .join(".mik")
        .join("bin");

    #[cfg(windows)]
    let wasm_tools_path = mik_bin.join("wasm-tools.exe");
    #[cfg(not(windows))]
    let wasm_tools_path = mik_bin.join("wasm-tools");

    if wasm_tools_path.exists() {
        return Ok(wasm_tools_path);
    }

    // Download wasm-tools
    println!("wasm-tools not found, downloading v{WASM_TOOLS_VERSION}...");
    download_wasm_tools(&mik_bin)?;

    Ok(wasm_tools_path)
}

/// Download wasm-tools for the current platform
fn download_wasm_tools(bin_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(bin_dir)?;

    let (os, arch, _ext) = get_platform_info()?;
    let archive_ext = if cfg!(windows) { "zip" } else { "tar.gz" };

    let url = format!(
        "https://github.com/bytecodealliance/wasm-tools/releases/download/v{WASM_TOOLS_VERSION}/wasm-tools-{WASM_TOOLS_VERSION}-{arch}-{os}.{archive_ext}"
    );

    println!("Downloading from: {url}");

    // Download using ureq (already a dependency)
    #[cfg(feature = "registry")]
    {
        let response = ureq::get(&url)
            .call()
            .context("Failed to download wasm-tools")?;

        let mut reader = response.into_body().into_reader();
        let temp_file = bin_dir.join(format!("wasm-tools-download.{archive_ext}"));

        {
            let mut file = std::fs::File::create(&temp_file)?;
            std::io::copy(&mut reader, &mut file)?;
        }

        // Extract
        if cfg!(windows) {
            extract_zip(&temp_file, bin_dir)?;
        } else {
            extract_tar_gz(&temp_file, bin_dir)?;
        }

        // Cleanup
        let _ = std::fs::remove_file(&temp_file);

        println!("Installed wasm-tools to {}", bin_dir.display());
        Ok(())
    }

    #[cfg(not(feature = "registry"))]
    {
        bail!(
            "wasm-tools not found. Install with: cargo install wasm-tools\n\
             Or download from: https://github.com/bytecodealliance/wasm-tools/releases"
        );
    }
}

fn get_platform_info() -> Result<(&'static str, &'static str, &'static str)> {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        bail!("Unsupported operating system");
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        bail!("Unsupported architecture");
    };

    let ext = if cfg!(windows) { ".exe" } else { "" };

    Ok((os, arch, ext))
}

#[cfg(feature = "registry")]
fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut archive = zip::ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name();

        // Look for wasm-tools binary
        if name.ends_with("wasm-tools.exe") || (name.ends_with("wasm-tools") && !name.contains('/'))
        {
            let out_path = dest.join("wasm-tools.exe");
            let mut out_file = std::fs::File::create(&out_path)?;
            std::io::copy(&mut file, &mut out_file)?;
            return Ok(());
        }

        // Handle nested directory structure
        if name.contains("wasm-tools") && (name.ends_with(".exe") || !name.contains('.')) {
            let Some(filename) = Path::new(name).file_name() else {
                continue; // Skip entries without valid filenames
            };
            let out_path = dest.join(filename);
            let mut out_file = std::fs::File::create(&out_path)?;
            std::io::copy(&mut file, &mut out_file)?;
        }
    }

    Ok(())
}

#[cfg(all(feature = "registry", not(windows)))]
fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = std::fs::File::open(archive)?;
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        // Look for wasm-tools binary
        if let Some(name) = path.file_name() {
            if name == "wasm-tools" {
                let out_path = dest.join("wasm-tools");
                entry.unpack(&out_path)?;

                // Make executable
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(&out_path)?.permissions();
                    perms.set_mode(0o755);
                    std::fs::set_permissions(&out_path, perms)?;
                }

                return Ok(());
            }
        }
    }

    Ok(())
}

#[cfg(all(feature = "registry", windows))]
const fn extract_tar_gz(_archive: &Path, _dest: &Path) -> Result<()> {
    // Windows uses zip, not tar.gz
    Ok(())
}

/// Execute the strip command
pub fn execute(input: &str, options: StripOptions) -> Result<()> {
    let input_path = Path::new(input);

    // Validate input exists
    if !input_path.exists() {
        bail!("Input file not found: {input}");
    }

    // Get wasm-tools (download if needed)
    let wasm_tools = get_wasm_tools()?;

    // Determine output path
    let output_path = options.output.as_ref().map_or_else(
        || {
            let stem = input_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("component");
            let parent = input_path.parent().unwrap_or_else(|| Path::new("."));
            parent
                .join(format!("{stem}.stripped.wasm"))
                .to_string_lossy()
                .to_string()
        },
        |out| out.clone(),
    );

    // Build wasm-tools strip command
    let mut cmd = Command::new(&wasm_tools);
    cmd.arg("strip");

    if options.all {
        cmd.arg("--all");
    } else {
        if options.debug {
            cmd.arg("--delete").arg(".debug*");
        }
        if options.names {
            cmd.arg("--delete").arg("name");
        }
        if options.producers {
            cmd.arg("--delete").arg("producers");
        }
    }

    cmd.arg(input);
    cmd.arg("-o").arg(&output_path);

    // Execute
    let output = cmd.output().context("Failed to execute wasm-tools strip")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("wasm-tools strip failed: {stderr}");
    }

    // Report results
    let input_size = std::fs::metadata(input)?.len();
    let output_size = std::fs::metadata(&output_path)?.len();
    let savings = input_size.saturating_sub(output_size);
    let percent = if input_size > 0 {
        (savings as f64 / input_size as f64) * 100.0
    } else {
        0.0
    };

    println!("Stripped: {input} -> {output_path}");
    println!(
        "Size: {} -> {} ({:.1}% smaller, saved {})",
        format_size(input_size),
        format_size(output_size),
        percent,
        format_size(savings)
    );

    Ok(())
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;

    if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes}B")
    }
}
