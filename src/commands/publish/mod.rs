//! Publish component to `GitHub` Releases.
//!
//! Creates a tar.gz archive with wasm, wit/, static/, and mik.toml.

mod archive;
mod discovery;
mod errors;
mod types;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::Manifest;
use crate::registry;

use archive::create_archive;
use discovery::{find_component, find_static_dir, find_wit_dir};
use errors::{handle_publish_error, handle_upload_error};

/// Publish component to `GitHub` Releases.
pub fn execute(tag: Option<&str>, dry_run: bool) -> Result<()> {
    let manifest = Manifest::load().context("No mik.toml found. Run 'mik init' first.")?;
    let name = &manifest.project.name;
    let version = tag.unwrap_or(&manifest.project.version);

    // Discover assets
    let component = find_component()?;
    let wit_dir = find_wit_dir();
    let static_dir = find_static_dir();
    let mik_toml = Path::new("mik.toml");

    // Print discovered assets
    println!("Component: {}", component.display());
    if let Some(ref w) = wit_dir {
        println!("WIT: {}", w.display());
    }
    if let Some(ref s) = static_dir {
        println!("Static: {}", s.display());
    }
    if mik_toml.exists() {
        println!("Manifest: mik.toml");
    }

    let repo = registry::get_git_repo()?;
    println!("Repository: {repo}");
    println!("Tag: {version}");

    if dry_run {
        print_dry_run(
            name,
            &repo,
            version,
            wit_dir.as_ref(),
            static_dir.as_ref(),
            mik_toml,
        );
        return Ok(());
    }

    // Create and publish archive
    fs::create_dir_all("target")?;
    let archive_path = Path::new("target").join(format!("{name}-{version}.tar.gz"));

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Creating archive...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    println!("\nCreating archive: {}", archive_path.display());
    create_archive(
        &archive_path,
        name,
        &component,
        wit_dir.as_deref(),
        static_dir.as_deref(),
        mik_toml,
    )?;

    spinner.set_message("Publishing to GitHub Releases...");

    let release_url = match registry::publish_github(&archive_path, &repo, version, None) {
        Ok(url) => {
            spinner.set_message("Release created successfully");
            url
        },
        Err(e) => {
            spinner.finish_and_clear();
            return handle_publish_error(e, &repo, version);
        },
    };

    // Upload raw .wasm for direct download
    let wasm_target = Path::new("target").join(format!("{name}.wasm"));
    fs::copy(&component, &wasm_target)?;

    spinner.set_message("Uploading component asset...");

    if let Err(e) = registry::upload_asset(&repo, version, &wasm_target) {
        spinner.finish_and_clear();
        return handle_upload_error(e, &repo, version);
    }

    spinner.finish_and_clear();

    println!("\nPublished: {release_url}");
    println!("\nOthers can use:\n  mik add {repo}");
    println!(
        "\nDirect download:\n  https://github.com/{repo}/releases/download/{version}/{name}.wasm"
    );

    Ok(())
}

/// Print dry-run summary.
fn print_dry_run(
    name: &str,
    repo: &str,
    version: &str,
    wit_dir: Option<&PathBuf>,
    static_dir: Option<&PathBuf>,
    mik_toml: &Path,
) {
    println!("\n[dry-run] Would publish to: https://github.com/{repo}/releases/tag/{version}");
    println!("\nArchive contents:");
    println!("  - {name}.wasm");
    if wit_dir.is_some() {
        println!("  - wit/*");
    }
    if static_dir.is_some() {
        println!("  - static/*");
    }
    if mik_toml.exists() {
        println!("  - mik.toml");
    }
    println!("\nTo publish for real, run without --dry-run");
}
