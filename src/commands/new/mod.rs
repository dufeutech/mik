//! Create a new mik project from templates.
//!
//! Scaffolds projects for multiple languages:
//! - Rust (default): mik-sdk based HTTP handlers
//! - `TypeScript`: jco + esbuild workflow
//!
//! WIT interfaces are fetched from OCI registry to ensure consistency with the bridge.

mod github;
mod interactive;
mod templates;

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

pub use templates::{Language, Template};

/// OCI reference for the WIT package.
const WIT_OCI_REF: &str = "ghcr.io/dufeutech/mik-sdk-wit";
/// WIT cache filename within the tools directory.
const WIT_CACHE_FILENAME: &str = "wit/core.wit";

/// Options for creating a new project.
#[derive(Debug, Clone)]
pub struct NewOptions {
    /// Project name
    pub name: String,
    /// Target language
    pub lang: Language,
    /// Template to use
    pub template: Template,
    /// Skip interactive prompts
    pub yes: bool,
    /// `GitHub` template (overrides lang/template)
    pub github_template: Option<String>,
}

impl Default for NewOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            lang: Language::Rust,
            template: Template::Basic,
            yes: false,
            github_template: None,
        }
    }
}

/// Create a new mik project.
pub async fn execute(options: NewOptions) -> Result<()> {
    let project_dir = Path::new(&options.name);

    // Extract just the directory name for the project name
    let project_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid project name: {}", options.name))?;

    if project_dir.exists() {
        anyhow::bail!("Directory '{}' already exists", options.name);
    }

    // Handle GitHub template
    if let Some(ref github_ref) = options.github_template {
        return github::create_from_github(project_dir, project_name, github_ref).await;
    }

    // Determine language and template (interactive or from options)
    let (lang, template) = if options.yes {
        (options.lang, options.template)
    } else if is_interactive() {
        interactive::prompt_options(options.lang, options.template)?
    } else {
        (options.lang, options.template)
    };

    println!("Creating new {lang} project: {project_name} (template: {template})");

    // Fetch WIT from OCI (or use cached version)
    let wit_content = fetch_wit().await?;

    // Create project directory
    fs::create_dir_all(project_dir).context("Failed to create project directory")?;

    // Get git user info
    let (git_name, git_email) = get_git_user();

    // Template context
    let ctx = templates::TemplateContext {
        project_name: project_name.to_string(),
        project_name_underscore: project_name.replace('-', "_"),
        author_name: git_name,
        author_email: git_email,
        year: chrono::Utc::now().format("%Y").to_string(),
        version: templates::DEFAULT_VERSION.to_string(),
    };

    // Generate project files from template
    templates::generate_project(project_dir, lang, template, &ctx, &wit_content)?;

    // Initialize git repository
    let _ = Command::new("git")
        .args(["init"])
        .current_dir(project_dir)
        .output();

    println!();
    println!("Created project: {project_name}");
    println!();

    // Print next steps based on language
    print_next_steps(project_name, lang);

    Ok(())
}

/// Get the WIT cache path: `~/.mik/tools/wit/core.wit`
fn get_wit_cache_path() -> Result<std::path::PathBuf> {
    Ok(crate::daemon::paths::get_tools_dir()?.join(WIT_CACHE_FILENAME))
}

/// Fetch WIT content from OCI registry or cache.
///
/// Discovery order:
/// 1. Check ~/.mik/tools/wit/core.wit (cached)
/// 2. Download from OCI registry and cache
async fn fetch_wit() -> Result<String> {
    // Check cached version first
    if let Ok(cache_path) = get_wit_cache_path()
        && cache_path.exists()
    {
        return fs::read_to_string(&cache_path).context("Failed to read cached WIT");
    }

    // Download from OCI (only when registry feature is enabled)
    #[cfg(feature = "registry")]
    {
        println!("Fetching WIT interface from registry...");
        let wit_content = download_wit().await?;

        // Cache for future use
        if let Ok(cache_path) = get_wit_cache_path() {
            if let Some(parent) = cache_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&cache_path, &wit_content);
        }

        Ok(wit_content)
    }

    #[cfg(not(feature = "registry"))]
    anyhow::bail!(
        "WIT interface not cached and registry feature is disabled.\n\n\
         The WIT interface is required for project scaffolding.\n\
         Options:\n\
         1. Rebuild mik with registry feature enabled\n\
         2. Manually place the WIT at ~/.mik/tools/wit/core.wit"
    )
}

/// Download WIT from OCI registry.
#[cfg(feature = "registry")]
async fn download_wit() -> Result<String> {
    let tools_dir = crate::daemon::paths::get_tools_dir()?;
    let wit_dir = tools_dir.join("wit");
    let temp_path = wit_dir.join("temp.wit");

    // Create directory if needed
    fs::create_dir_all(&wit_dir).context("Failed to create WIT cache directory")?;

    // Use pull_oci to download
    super::pull::pull_oci(WIT_OCI_REF, &temp_path)
        .await
        .context("Failed to download WIT from registry")?;

    // Read content
    let content = fs::read_to_string(&temp_path).context("Failed to read downloaded WIT")?;

    // Move to final location
    let final_path = get_wit_cache_path()?;
    fs::rename(&temp_path, &final_path)
        .or_else(|_| fs::copy(&temp_path, &final_path).map(|_| ()))
        .context("Failed to cache WIT")?;

    Ok(content)
}

/// Print next steps based on language.
fn print_next_steps(project_name: &str, lang: Language) {
    println!("Next steps:");
    println!("  cd {project_name}");

    match lang {
        Language::Rust => {
            println!("  mik build -rc");
            println!("  mik run");
        },
        Language::TypeScript => {
            println!("  npm install");
            println!("  npm run build");
            println!("  mik run {project_name}.wasm");
        },
    }

    println!();
    println!("Documentation: https://dufeutech.github.io/mik/guides/building-components/");
}

/// Check if running in interactive mode (TTY).
fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Get git user.name and user.email from git config.
fn get_git_user() -> (Option<String>, Option<String>) {
    let name = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let email = Command::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    (name, email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_options() {
        let opts = NewOptions::default();
        assert_eq!(opts.lang, Language::Rust);
        assert_eq!(opts.template, Template::Basic);
        assert!(!opts.yes);
    }
}
