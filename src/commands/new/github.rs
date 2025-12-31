//! `GitHub` template fetching.
//!
//! Supports:
//! - `github:user/repo`
//! - `github:user/repo#branch`

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Create a project from a `GitHub` template.
///
/// # Arguments
/// * `dir` - Target directory
/// * `name` - Project name
/// * `github_ref` - `GitHub` reference (e.g., `github:user/repo` or `github:user/repo#branch`)
pub fn create_from_github(dir: &Path, name: &str, github_ref: &str) -> Result<()> {
    let (repo, branch) = parse_github_ref(github_ref)?;

    println!("Cloning template from GitHub: {repo}");
    if let Some(ref b) = branch {
        println!("  Branch: {b}");
    }

    // Clone repository
    clone_repository(dir, &repo, branch.as_deref())?;

    // Replace template variables in files
    replace_variables(dir, name)?;

    // Remove .git directory
    let git_dir = dir.join(".git");
    if git_dir.exists() {
        fs::remove_dir_all(&git_dir).ok();
    }

    // Initialize new git repository
    let _ = Command::new("git").args(["init"]).current_dir(dir).output();

    println!();
    println!("Created project from template: {name}");
    println!();
    println!("Next steps:");
    println!("  cd {name}");
    println!("  # Follow README.md for build instructions");

    Ok(())
}

/// Parse `GitHub` reference into (`repo_url`, `optional_branch`).
fn parse_github_ref(github_ref: &str) -> Result<(String, Option<String>)> {
    // Remove "github:" prefix if present
    let cleaned = github_ref.strip_prefix("github:").unwrap_or(github_ref);

    // Split on # to get branch
    let (repo_part, branch) = if let Some(idx) = cleaned.find('#') {
        let (repo, branch) = cleaned.split_at(idx);
        (repo, Some(branch[1..].to_string()))
    } else {
        (cleaned, None)
    };

    // Validate repo format (user/repo)
    if !repo_part.contains('/') || repo_part.starts_with('/') || repo_part.ends_with('/') {
        anyhow::bail!(
            "Invalid GitHub reference: {github_ref}\n\
             Expected format: github:user/repo or github:user/repo#branch"
        );
    }

    let repo_url = format!("https://github.com/{repo_part}.git");

    Ok((repo_url, branch))
}

/// Clone a git repository.
fn clone_repository(dir: &Path, repo_url: &str, branch: Option<&str>) -> Result<()> {
    let mut args = vec!["clone", "--depth=1"];

    if let Some(b) = branch {
        args.push("--branch");
        args.push(b);
    }

    args.push(repo_url);
    args.push(dir.to_str().context("Invalid directory path")?);

    let output = Command::new("git")
        .args(&args)
        .output()
        .context("Failed to run git clone. Is git installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git clone failed: {stderr}");
    }

    Ok(())
}

/// Replace template variables in all files.
fn replace_variables(dir: &Path, project_name: &str) -> Result<()> {
    let project_name_underscore = project_name.replace('-', "_");

    // Walk directory and replace variables in text files
    for entry in walkdir(dir)? {
        let path = entry;

        // Skip binary files and .git
        if path.starts_with(dir.join(".git")) {
            continue;
        }

        // Only process text files
        if let Ok(content) = fs::read_to_string(&path) {
            let modified = content
                .replace("{{PROJECT_NAME}}", project_name)
                .replace("{{PROJECT_NAME_UNDERSCORE}}", &project_name_underscore);

            if modified != content {
                fs::write(&path, modified)?;
            }
        }
    }

    Ok(())
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
        let (url, branch) = parse_github_ref("github:user/repo").unwrap();
        assert_eq!(url, "https://github.com/user/repo.git");
        assert!(branch.is_none());
    }

    #[test]
    fn test_parse_github_ref_with_branch() {
        let (url, branch) = parse_github_ref("github:user/repo#main").unwrap();
        assert_eq!(url, "https://github.com/user/repo.git");
        assert_eq!(branch, Some("main".to_string()));
    }

    #[test]
    fn test_parse_github_ref_without_prefix() {
        let (url, branch) = parse_github_ref("user/repo#develop").unwrap();
        assert_eq!(url, "https://github.com/user/repo.git");
        assert_eq!(branch, Some("develop".to_string()));
    }

    #[test]
    fn test_parse_github_ref_invalid() {
        assert!(parse_github_ref("invalid").is_err());
        assert!(parse_github_ref("/repo").is_err());
        assert!(parse_github_ref("user/").is_err());
    }
}
