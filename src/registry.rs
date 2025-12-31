//! Registry operations for WASM components.
//!
//! Downloads from any HTTP(S) URL, publishes to `GitHub` Releases via `gh` CLI.

use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// HTTP timeout for receiving response body
const HTTP_RECV_BODY_TIMEOUT_SECS: u64 = 60;
/// HTTP connect timeout
const HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;

/// `GitHub` domain patterns for authentication.
const GITHUB_DOMAINS: [&str; 2] = ["github.com", "githubusercontent.com"];

/// Download a file from any HTTP(S) URL.
pub fn download(url: &str, output_path: &Path) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let body = download_from_url(url)?;
    let mut file = File::create(output_path).context("Failed to create output file")?;
    io::copy(&mut body.into_reader(), &mut file)?;

    Ok(())
}

/// Download and extract a .tar.gz archive.
#[allow(dead_code)]
pub fn download_and_extract(url: &str, output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir)?;

    let body = download_from_url(url)?;
    let gz = flate2::read::GzDecoder::new(body.into_reader());
    tar::Archive::new(gz)
        .unpack(output_dir)
        .context("Failed to extract archive")?;

    Ok(())
}

/// Download from URL, handling `GitHub` authentication.
fn download_from_url(url: &str) -> Result<ureq::Body> {
    let config = ureq::Agent::config_builder()
        .timeout_recv_body(Some(Duration::from_secs(HTTP_RECV_BODY_TIMEOUT_SECS)))
        .timeout_connect(Some(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECS)))
        .build();
    let agent: ureq::Agent = config.into();
    let mut request = agent.get(url).header("Accept", "application/octet-stream");

    if is_github_url(url)
        && let Some(token) = get_github_token()
    {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }

    Ok(request
        .call()
        .context("Failed to make HTTP request")?
        .into_body())
}

/// Check if URL is a `GitHub` domain.
fn is_github_url(url: &str) -> bool {
    GITHUB_DOMAINS.iter().any(|domain| url.contains(domain))
}

/// Publish a component to `GitHub` Releases.
pub fn publish_github(
    asset_path: &Path,
    repo: &str,
    tag: &str,
    notes: Option<&str>,
) -> Result<String> {
    check_gh_auth()?;

    let notes_args: Vec<String> = match notes {
        Some(n) => vec!["--notes".into(), n.into()],
        None => vec!["--generate-notes".into()],
    };

    let output = Command::new("gh")
        .args(["release", "create", tag])
        .arg(asset_path)
        .args(["--repo", repo, "--title", tag])
        .args(&notes_args)
        .output()
        .context("Failed to run gh release create")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to create release: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(format!("https://github.com/{repo}/releases/tag/{tag}"))
}

/// Upload an asset to an existing `GitHub` release.
pub fn upload_asset(repo: &str, tag: &str, asset_path: &Path) -> Result<()> {
    check_gh_auth()?;

    let output = Command::new("gh")
        .args(["release", "upload", tag])
        .arg(asset_path)
        .args(["--repo", repo, "--clobber"])
        .output()
        .context("Failed to upload asset")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to upload asset: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Verify gh CLI is authenticated.
fn check_gh_auth() -> Result<()> {
    Command::new("gh")
        .args(["auth", "status"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| ())
        .ok_or_else(|| anyhow::anyhow!("Not authenticated with GitHub. Run: gh auth login"))
}

/// Get `GitHub` token from gh CLI or `GITHUB_TOKEN` env var.
fn get_github_token() -> Option<String> {
    Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let token = String::from_utf8_lossy(&o.stdout).trim().to_string();
            (!token.is_empty()).then_some(token)
        })
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
}

/// Get the git repository in owner/repo format from origin remote.
pub fn get_git_repo() -> Result<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .context("Failed to get git remote")?;

    if !output.status.success() {
        anyhow::bail!("Not in a git repository with origin remote");
    }

    parse_git_url(String::from_utf8_lossy(&output.stdout).trim())
}

/// Parse git URL to extract owner/repo.
fn parse_git_url(url_str: &str) -> Result<String> {
    let url_str = url_str.trim_end_matches(".git");

    // Try standard URL parsing first (handles https://, http://, file://, etc.)
    if let Ok(parsed) = url::Url::parse(url_str) {
        match parsed.scheme() {
            // Reject file:// URLs - not a remote repository
            "file" => {
                anyhow::bail!("file:// URLs are not supported - not a remote repository");
            },
            // Handle standard HTTPS/HTTP URLs
            "https" | "http" => {
                let path = parsed.path().trim_start_matches('/').trim_end_matches('/');
                let segments: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();

                if segments.len() >= 2 {
                    let owner = segments[segments.len() - 2];
                    let repo = segments[segments.len() - 1];
                    return Ok(format!("{owner}/{repo}"));
                }
            },
            // Handle SSH URLs (ssh://git@github.com/owner/repo)
            "ssh" => {
                let path = parsed.path().trim_start_matches('/').trim_end_matches('/');
                let segments: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();

                if segments.len() >= 2 {
                    let owner = segments[segments.len() - 2];
                    let repo = segments[segments.len() - 1];
                    return Ok(format!("{owner}/{repo}"));
                }
            },
            _ => {
                // Fall through to try other parsing methods
            },
        }
    }

    // Handle SCP-style URLs: git@github.com:owner/repo
    if url_str.contains('@')
        && url_str.contains(':')
        && !url_str.contains("://")
        && let Some(colon_pos) = url_str.rfind(':')
    {
        let path = &url_str[colon_pos + 1..];
        let segments: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();

        if segments.len() >= 2 {
            let owner = segments[segments.len() - 2];
            let repo = segments[segments.len() - 1];
            return Ok(format!("{owner}/{repo}"));
        }
    }

    anyhow::bail!("Could not parse repository from: {url_str}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_url_https() {
        assert_eq!(
            parse_git_url("https://github.com/owner/repo").unwrap(),
            "owner/repo"
        );
        assert_eq!(
            parse_git_url("https://github.com/owner/repo.git").unwrap(),
            "owner/repo"
        );
    }

    #[test]
    fn test_parse_git_url_ssh_scp_style() {
        assert_eq!(
            parse_git_url("git@github.com:owner/repo").unwrap(),
            "owner/repo"
        );
        assert_eq!(
            parse_git_url("git@github.com:owner/repo.git").unwrap(),
            "owner/repo"
        );
    }

    #[test]
    fn test_parse_git_url_ssh_url_style() {
        assert_eq!(
            parse_git_url("ssh://git@github.com/owner/repo").unwrap(),
            "owner/repo"
        );
        assert_eq!(
            parse_git_url("ssh://git@github.com/owner/repo.git").unwrap(),
            "owner/repo"
        );
    }

    #[test]
    fn test_parse_git_url_file_rejected() {
        let result = parse_git_url("file:///path/to/repo");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a remote repository")
        );
    }

    #[test]
    fn test_parse_git_url_nested_paths() {
        assert_eq!(
            parse_git_url("https://gitlab.com/group/subgroup/owner/repo").unwrap(),
            "owner/repo"
        );
    }

    #[test]
    fn test_parse_git_url_invalid() {
        assert!(parse_git_url("invalid-url").is_err());
        assert!(parse_git_url("https://github.com/").is_err());
        assert!(parse_git_url("https://github.com/owner").is_err());
    }

    #[test]
    fn test_parse_git_url_http() {
        assert_eq!(
            parse_git_url("http://github.com/owner/repo").unwrap(),
            "owner/repo"
        );
    }
}
