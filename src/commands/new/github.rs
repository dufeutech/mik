//! `GitHub` template fetching via HTTP.
//!
//! Downloads templates as zip archives from GitHub - no git CLI required.
//!
//! Supports:
//! - `github:user/repo`
//! - `github:user/repo#branch`
//! - `user/repo` (shorthand)

use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::process::Command;

/// Create a project from a `GitHub` template.
///
/// Downloads the repository as a zip archive and extracts it.
/// No git CLI required - uses HTTP to fetch the zipball.
///
/// # Arguments
/// * `dir` - Target directory
/// * `name` - Project name
/// * `github_ref` - `GitHub` reference (e.g., `github:user/repo` or `github:user/repo#branch`)
pub async fn create_from_github(dir: &Path, name: &str, github_ref: &str) -> Result<()> {
    let (owner, repo, branch) = parse_github_ref(github_ref)?;

    let branch_display = branch.as_deref().unwrap_or("main");
    println!("Downloading template: {owner}/{repo}");
    println!("  Branch: {branch_display}");

    // Download and extract
    download_and_extract(&owner, &repo, branch.as_deref(), dir).await?;

    // Replace template variables in files
    replace_variables(dir, name)?;

    // Initialize new git repository (optional, doesn't fail if git not installed)
    let _ = Command::new("git").args(["init"]).current_dir(dir).output();

    println!();
    println!("Created project: {name}");
    println!();
    println!("Next steps:");
    println!("  cd {name}");
    println!("  # Follow README.md for build instructions");

    Ok(())
}

/// Parse `GitHub` reference into (owner, repo, optional_branch).
fn parse_github_ref(github_ref: &str) -> Result<(String, String, Option<String>)> {
    // Remove "github:" prefix if present
    let cleaned = github_ref.strip_prefix("github:").unwrap_or(github_ref);

    // Split on # to get branch
    let (repo_part, branch) = cleaned.find('#').map_or((cleaned, None), |idx| {
        let (repo, branch) = cleaned.split_at(idx);
        (repo, Some(branch[1..].to_string()))
    });

    // Parse owner/repo
    let parts: Vec<&str> = repo_part.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        anyhow::bail!(
            "Invalid GitHub reference: {github_ref}\n\
             Expected format: github:user/repo or github:user/repo#branch"
        );
    }

    Ok((parts[0].to_string(), parts[1].to_string(), branch))
}

/// Download repository as zip and extract to target directory.
async fn download_and_extract(
    owner: &str,
    repo: &str,
    branch: Option<&str>,
    target_dir: &Path,
) -> Result<()> {
    let branch = branch.unwrap_or("main");

    // GitHub zipball URL
    // Format: https://github.com/{owner}/{repo}/archive/refs/heads/{branch}.zip
    let url = format!("https://github.com/{owner}/{repo}/archive/refs/heads/{branch}.zip");

    // Download zip
    let response = reqwest::get(&url)
        .await
        .context("Failed to connect to GitHub")?;

    if !response.status().is_success() {
        // Try with 'master' if 'main' failed and no explicit branch was set
        if branch == "main" {
            let fallback_url =
                format!("https://github.com/{owner}/{repo}/archive/refs/heads/master.zip");

            let fallback = reqwest::get(&fallback_url).await?;

            if fallback.status().is_success() {
                return extract_zip_response(fallback, target_dir, repo, "master").await;
            }
        }

        anyhow::bail!(
            "Failed to download template: HTTP {}\n\
             URL: {url}\n\
             Make sure the repository exists and is public.",
            response.status()
        );
    }

    extract_zip_response(response, target_dir, repo, branch).await
}

/// Extract zip from HTTP response to target directory.
async fn extract_zip_response(
    response: reqwest::Response,
    target_dir: &Path,
    repo: &str,
    branch: &str,
) -> Result<()> {
    let bytes = response
        .bytes()
        .await
        .context("Failed to download zip content")?;

    // Create a temporary directory for extraction
    let temp_dir = target_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!(".mik-template-{}", std::process::id()));

    // Clean up temp dir if it exists
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }
    fs::create_dir_all(&temp_dir)?;

    // Extract zip to temp directory
    extract_zip(&bytes, &temp_dir)?;

    // GitHub zips contain a single folder: {repo}-{branch}/
    // We need to move its contents to the target directory
    let inner_dir = temp_dir.join(format!("{repo}-{branch}"));

    if !inner_dir.exists() {
        // Try to find the actual directory (branch might have different name)
        let mut found_dir = None;
        for entry in fs::read_dir(&temp_dir)? {
            let entry = entry?;
            if entry.path().is_dir() {
                found_dir = Some(entry.path());
                break;
            }
        }

        if let Some(dir) = found_dir {
            move_contents(&dir, target_dir)?;
        } else {
            anyhow::bail!("Unexpected zip structure - no inner directory found");
        }
    } else {
        move_contents(&inner_dir, target_dir)?;
    }

    // Clean up temp directory
    fs::remove_dir_all(&temp_dir).ok();

    Ok(())
}

/// Extract zip bytes to a directory.
fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).context("Invalid zip archive")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = dest.join(file.mangled_name());

        if file.name().ends_with('/') {
            // Directory
            fs::create_dir_all(&outpath)?;
        } else {
            // File
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut outfile = File::create(&outpath)?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)?;
            outfile.write_all(&buffer)?;
        }

        // Set permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                fs::set_permissions(&outpath, fs::Permissions::from_mode(mode))?;
            }
        }
    }

    Ok(())
}

/// Move contents from source directory to target directory.
fn move_contents(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dest_path = dest.join(&file_name);

        if src_path.is_dir() {
            // Recursively copy directory
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            // Copy file
            fs::copy(&src_path, &dest_path)?;
        }
    }

    Ok(())
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            fs::copy(&src_path, &dest_path)?;
        }
    }

    Ok(())
}

/// Replace template variables in all files.
fn replace_variables(dir: &Path, project_name: &str) -> Result<()> {
    let project_name_underscore = project_name.replace('-', "_");

    // Get git user info for author
    let author_name = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let author_email = Command::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    // Walk directory and replace variables in text files
    for path in walkdir(dir)? {
        // Skip binary-like extensions
        if is_binary_file(&path) {
            continue;
        }

        // Only process text files
        if let Ok(content) = fs::read_to_string(&path) {
            let modified = content
                .replace("{{PROJECT_NAME}}", project_name)
                .replace("{{PROJECT_NAME_UNDERSCORE}}", &project_name_underscore)
                .replace("{{AUTHOR_NAME}}", author_name.as_deref().unwrap_or(""))
                .replace("{{AUTHOR_EMAIL}}", author_email.as_deref().unwrap_or(""));

            if modified != content {
                fs::write(&path, modified)?;
            }
        }
    }

    Ok(())
}

/// Check if a file is likely binary based on extension.
fn is_binary_file(path: &Path) -> bool {
    let binary_extensions = [
        "wasm", "png", "jpg", "jpeg", "gif", "ico", "pdf", "zip", "tar", "gz", "exe", "dll", "so",
        "dylib", "o", "a", "bin", "dat",
    ];

    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| binary_extensions.contains(&ext.to_lowercase().as_str()))
}

/// Simple recursive directory walker.
fn walkdir(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            files.extend(walkdir(&path)?);
        } else if path.is_file() {
            files.push(path);
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_ref_simple() {
        let (owner, repo, branch) = parse_github_ref("github:user/repo").unwrap();
        assert_eq!(owner, "user");
        assert_eq!(repo, "repo");
        assert!(branch.is_none());
    }

    #[test]
    fn test_parse_github_ref_with_branch() {
        let (owner, repo, branch) = parse_github_ref("github:user/repo#main").unwrap();
        assert_eq!(owner, "user");
        assert_eq!(repo, "repo");
        assert_eq!(branch, Some("main".to_string()));
    }

    #[test]
    fn test_parse_github_ref_without_prefix() {
        let (owner, repo, branch) = parse_github_ref("user/repo#develop").unwrap();
        assert_eq!(owner, "user");
        assert_eq!(repo, "repo");
        assert_eq!(branch, Some("develop".to_string()));
    }

    #[test]
    fn test_parse_github_ref_invalid() {
        assert!(parse_github_ref("invalid").is_err());
        assert!(parse_github_ref("/repo").is_err());
        assert!(parse_github_ref("user/").is_err());
    }

    #[test]
    fn test_is_binary_file() {
        assert!(is_binary_file(Path::new("test.wasm")));
        assert!(is_binary_file(Path::new("image.png")));
        assert!(!is_binary_file(Path::new("code.rs")));
        assert!(!is_binary_file(Path::new("config.toml")));
    }
}
