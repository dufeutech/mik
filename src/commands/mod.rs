#[cfg(feature = "registry")]
pub mod add;
pub mod build;
pub mod cache;
pub mod daemon;
pub mod new;
#[cfg(feature = "registry")]
pub mod publish;
#[cfg(feature = "registry")]
pub mod pull;
pub mod run;
pub mod static_cmd;

use anyhow::{Context, Result};
use std::process::Command;

/// Check if a command-line tool is available
pub fn check_tool(name: &str) -> Result<()> {
    let output = Command::new(name).arg("--version").output();

    match output {
        Ok(output) if output.status.success() => Ok(()),
        _ => anyhow::bail!("Required tool '{name}' not found. Please install it to continue."),
    }
}

/// Run a command and capture output
#[allow(dead_code)]
pub fn run_command(program: &str, args: &[&str]) -> Result<()> {
    println!("Running: {} {}", program, args.join(" "));

    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("Failed to execute '{program}'"))?;

    if !status.success() {
        anyhow::bail!("Command failed with status: {status}");
    }

    Ok(())
}

/// Get the absolute path to a directory
#[allow(dead_code)]
pub fn get_absolute_path(path: &str) -> Result<String> {
    let path = std::path::Path::new(path);
    let absolute = if path.is_relative() {
        std::env::current_dir()?.join(path)
    } else {
        path.to_path_buf()
    };

    Ok(absolute
        .to_str()
        .context("Path contains invalid UTF-8")?
        .to_string())
}
