//! Add dependencies to mik.toml and download them.
//!
//! Supports:
//! - `ghcr.io/user/repo:tag` -> OCI registry
//! - `user/repo` -> OCI shorthand for ghcr.io/user/repo:latest
//! - `user/repo:tag` -> OCI shorthand with tag
//! - `https://example.com/component.wasm` -> HTTP URL (private or public)
//! - `--git <url>` -> Git repository
//! - `--path <dir>` -> Local path

use super::pull;
use crate::manifest::{Dependency, DependencyDetail, Manifest};
use anyhow::{Context, Result};

/// Add a dependency to the project.
pub async fn execute(
    packages: &[String],
    git: Option<&str>,
    path: Option<&str>,
    tag: Option<&str>,
    branch: Option<&str>,
    dev: bool,
) -> Result<()> {
    if packages.is_empty() {
        anyhow::bail!("No packages specified");
    }

    let mut manifest = Manifest::load().context("No mik.toml found. Run 'mik init' first.")?;

    for package in packages {
        let (name, dep) = parse_and_create_dep(package, git, path, tag, branch)?;

        let dep_type = if dev { "dev-dependency" } else { "dependency" };

        if dev {
            manifest.dev_dependencies.insert(name.clone(), dep.clone());
        } else {
            manifest.dependencies.insert(name.clone(), dep.clone());
        }

        println!("Added {} as {}: {}", name, dep_type, format_dep(&dep));
    }

    manifest.save()?;
    println!();
    println!("Updated mik.toml");

    // Auto-pull the added dependencies
    println!();
    println!("Downloading...");
    pull::sync().await?;

    Ok(())
}

/// Parse package spec and create dependency.
fn parse_and_create_dep(
    package: &str,
    git: Option<&str>,
    path: Option<&str>,
    tag: Option<&str>,
    branch: Option<&str>,
) -> Result<(String, Dependency)> {
    // Explicit --git flag
    if let Some(git_url) = git {
        let name = extract_name_from_url(git_url).unwrap_or(package);
        return Ok((
            name.to_string(),
            Dependency::Detailed(DependencyDetail {
                git: Some(git_url.to_string()),
                tag: tag.map(std::string::ToString::to_string),
                branch: branch.map(std::string::ToString::to_string),
                ..Default::default()
            }),
        ));
    }

    // Explicit --path flag
    if let Some(local_path) = path {
        let name = std::path::Path::new(local_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(package);
        return Ok((
            name.to_string(),
            Dependency::Detailed(DependencyDetail {
                path: Some(local_path.to_string()),
                ..Default::default()
            }),
        ));
    }

    // HTTP/HTTPS URL (private or public)
    if package.starts_with("http://") || package.starts_with("https://") {
        let name = extract_name_from_url(package)
            .ok_or_else(|| anyhow::anyhow!("Could not extract name from URL: {package}"))?;
        return Ok((
            name.to_string(),
            Dependency::Detailed(DependencyDetail {
                registry: Some(package.to_string()),
                ..Default::default()
            }),
        ));
    }

    // OCI format: user/repo, user/repo:tag, or full registry like ghcr.io/user/repo:tag
    if package.contains('/') {
        let (name, oci_ref) = parse_oci_reference(package, tag)?;
        return Ok((
            name,
            Dependency::Detailed(DependencyDetail {
                registry: Some(oci_ref),
                ..Default::default()
            }),
        ));
    }

    // No valid format
    anyhow::bail!(
        "Invalid package '{package}'. Use one of:\n  \
         mik add user/repo              # OCI (ghcr.io/user/repo:latest)\n  \
         mik add user/repo:v1.0         # OCI with tag\n  \
         mik add ghcr.io/user/repo:tag  # Full OCI reference\n  \
         mik add https://host/pkg.wasm  # HTTP URL\n  \
         mik add pkg --path ../pkg      # Local path\n  \
         mik add pkg --git <url>        # Git repository"
    );
}

/// Parse OCI reference and return (name, `full_oci_reference`).
///
/// Supports:
/// - `user/repo` -> `ghcr.io/user/repo:latest`
/// - `user/repo:tag` -> `ghcr.io/user/repo:tag`
/// - `ghcr.io/user/repo:tag` -> as-is
/// - `docker.io/user/repo:tag` -> as-is
fn parse_oci_reference(spec: &str, explicit_tag: Option<&str>) -> Result<(String, String)> {
    // Check if it's already a full registry reference (contains a dot before first /)
    let is_full_ref = spec
        .find('/')
        .is_some_and(|slash_pos| spec[..slash_pos].contains('.'));

    let (registry, path, tag) = if is_full_ref {
        // Full reference: ghcr.io/user/repo:tag or docker.io/library/image:tag
        let (ref_without_tag, tag) = split_tag(spec);
        let Some(first_slash) = ref_without_tag.find('/') else {
            // This shouldn't happen due to is_full_ref check, but handle gracefully
            return Err(anyhow::anyhow!(
                "Invalid OCI reference: missing path separator"
            ));
        };
        let registry = &ref_without_tag[..first_slash];
        let path = &ref_without_tag[first_slash + 1..];
        (registry, path, tag)
    } else {
        // Short reference: user/repo or user/repo:tag -> default to ghcr.io
        let (path, tag) = split_tag(spec);
        ("ghcr.io", path, tag)
    };

    // Use explicit tag if provided, otherwise use parsed tag or "latest"
    let final_tag = explicit_tag.unwrap_or_else(|| tag.unwrap_or("latest"));

    // Extract name (last path segment)
    let name = path
        .rsplit('/')
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid OCI reference: {spec}"))?;

    let full_ref = format!("{registry}/{path}:{final_tag}");

    Ok((name.to_string(), full_ref))
}

/// Split tag from reference: "foo/bar:tag" -> ("foo/bar", Some("tag"))
fn split_tag(spec: &str) -> (&str, Option<&str>) {
    // Find the last colon that's after the last slash (to avoid matching port numbers)
    if let Some(last_slash) = spec.rfind('/') {
        if let Some(colon) = spec[last_slash..].rfind(':') {
            let colon_pos = last_slash + colon;
            return (&spec[..colon_pos], Some(&spec[colon_pos + 1..]));
        }
    } else if let Some(colon) = spec.rfind(':') {
        return (&spec[..colon], Some(&spec[colon + 1..]));
    }
    (spec, None)
}

/// Extract component name from URL.
fn extract_name_from_url(url: &str) -> Option<&str> {
    url.rsplit('/')
        .next()
        .map(|s| s.trim_end_matches(".wasm"))
        .filter(|s| !s.is_empty())
}

/// Format dependency for display.
fn format_dep(dep: &Dependency) -> String {
    match dep {
        Dependency::Simple(v) => format!("version {v}"),
        Dependency::Detailed(d) => {
            if let Some(git) = &d.git {
                format!("git: {git}")
            } else if let Some(path) = &d.path {
                format!("path: {path}")
            } else if let Some(reg) = &d.registry {
                let version = d
                    .version
                    .as_deref()
                    .or(d.tag.as_deref())
                    .unwrap_or("latest");
                format!("{reg} @ {version}")
            } else {
                d.version.clone().unwrap_or_else(|| "latest".to_string())
            }
        },
    }
}

/// Remove a dependency from the project.
pub fn remove(packages: &[String], dev: bool) -> Result<()> {
    if packages.is_empty() {
        anyhow::bail!("No packages specified");
    }

    let mut manifest = Manifest::load().context("No mik.toml found.")?;

    for package in packages {
        let removed = if dev {
            manifest.dev_dependencies.remove(package)
        } else {
            manifest.dependencies.remove(package)
        };

        if removed.is_some() {
            println!("Removed {package}");
        } else {
            println!("Package {package} not found");
        }
    }

    manifest.save()?;
    println!();
    println!("Updated mik.toml");

    Ok(())
}
