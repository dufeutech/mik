//! Create a new mik project from templates.
//!
//! Scaffolds projects for multiple languages:
//! - Rust (default): mik-sdk based HTTP handlers
//! - TypeScript: jco + esbuild workflow

mod github;
mod interactive;
mod templates;

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

pub use templates::{Language, Template};

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
    /// GitHub template (overrides lang/template)
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
pub fn execute(options: NewOptions) -> Result<()> {
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
        return github::create_from_github(project_dir, project_name, github_ref);
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

    // Create project directory
    fs::create_dir_all(project_dir).context("Failed to create project directory")?;

    // Get git user info
    let (git_name, git_email) = get_git_user();

    // Template context
    let ctx = templates::TemplateContext {
        project_name: project_name.to_string(),
        project_name_underscore: project_name.replace('-', "_"),
        author_name: git_name.clone(),
        author_email: git_email.clone(),
        year: chrono::Utc::now().format("%Y").to_string(),
        version: templates::DEFAULT_VERSION.to_string(),
    };

    // Generate project files from template
    templates::generate_project(project_dir, lang, template, &ctx)?;

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
    println!("Documentation: https://dufeut.github.io/mik/guides/building-components/");
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
