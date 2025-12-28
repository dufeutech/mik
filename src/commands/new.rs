//! Create a new mik project from scratch.
//!
//! Scaffolds a complete project with:
//! - Cargo.toml with component metadata
//! - mik.toml with composition enabled
//! - WIT files for the handler interface
//! - src/lib.rs with a hello world handler

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::manifest::{Author, CompositionConfig, Manifest, Project, ServerConfig};

/// Create a new mik project.
pub fn execute(name: &str, is_lib: bool) -> Result<()> {
    let project_dir = Path::new(name);

    // Extract just the directory name for the project name
    let project_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid project name: {}", name))?;

    if project_dir.exists() {
        anyhow::bail!("Directory '{}' already exists", name);
    }

    println!("Creating new mik project: {project_name}");

    // Create project directory
    fs::create_dir_all(project_dir).context("Failed to create project directory")?;

    // Create subdirectories
    fs::create_dir_all(project_dir.join("src"))?;
    fs::create_dir_all(project_dir.join("wit/deps/core"))?;

    // Get git user info
    let (git_name, git_email) = get_git_user();

    // Generate project files
    write_cargo_toml(project_dir, project_name)?;
    write_mik_toml(
        project_dir,
        project_name,
        is_lib,
        git_name.as_deref(),
        git_email.as_deref(),
    )?;
    write_lib_rs(project_dir, project_name)?;
    write_wit_files(project_dir, project_name)?;

    // Create modules directory with .gitkeep (required by mik run)
    fs::create_dir_all(project_dir.join("modules"))?;
    fs::write(project_dir.join("modules/.gitkeep"), "")?;

    // Initialize git repository
    let _ = Command::new("git")
        .args(["init"])
        .current_dir(project_dir)
        .output();

    // Create .gitignore
    write_gitignore(project_dir)?;

    println!();
    println!("Created project: {project_name}");
    println!();
    println!("Next steps:");
    println!("  cd {project_name}");
    println!("  mik build -rc");
    println!("  mik run");

    Ok(())
}

/// Write Cargo.toml with component metadata.
fn write_cargo_toml(dir: &Path, name: &str) -> Result<()> {
    let content = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen-rt = "0.44.0"
mik-sdk = {{ git = "https://github.com/dufeut/mik-sdk", default-features = false }}

[package.metadata.component]
package = "mik:{name}"

[package.metadata.component.target]
path = "wit"
world = "{name}"

[package.metadata.component.target.dependencies]
"mik:core" = {{ path = "wit/deps/core" }}
"#
    );

    fs::write(dir.join("Cargo.toml"), content)?;
    Ok(())
}

/// Write mik.toml manifest.
fn write_mik_toml(
    dir: &Path,
    name: &str,
    is_lib: bool,
    git_name: Option<&str>,
    git_email: Option<&str>,
) -> Result<()> {
    let project_type = if is_lib { "lib" } else { "app" };

    let manifest = Manifest {
        project: Project {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            backend: "2".to_string(),
            description: Some(format!("A WASI HTTP {project_type}")),
            license: Some("MIT".to_string()),
            authors: if let Some(author_name) = git_name {
                vec![Author {
                    name: author_name.to_string(),
                    email: git_email.map(std::string::ToString::to_string),
                }]
            } else {
                vec![]
            },
            keywords: vec![],
            urls: None,
            r#type: project_type.to_string(),
        },
        server: ServerConfig {
            port: 3000,
            modules: "modules/".to_string(),
            ..Default::default()
        },
        composition: CompositionConfig {
            http_handler: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let toml_content = toml::to_string_pretty(&manifest)?;
    fs::write(dir.join("mik.toml"), toml_content)?;
    Ok(())
}

/// Write src/lib.rs with hello world handler.
fn write_lib_rs(dir: &Path, name: &str) -> Result<()> {
    let content = format!(
        r##"//! {name} - A mik HTTP handler

#[allow(warnings)]
mod bindings;

use bindings::exports::mik::core::handler::{{self, Guest, Response}};
use mik_sdk::prelude::*;

routes! {{
    GET "/" | "" => home,
    GET "/health" => health,
}}

fn home(_req: &Request) -> Response {{
    ok!({{
        "service": "{name}",
        "message": "Hello from mik!"
    }})
}}

fn health(_req: &Request) -> Response {{
    ok!({{ "status": "healthy" }})
}}
"##
    );

    fs::write(dir.join("src/lib.rs"), content)?;
    Ok(())
}

/// Write WIT interface files.
fn write_wit_files(dir: &Path, name: &str) -> Result<()> {
    // Main world.wit
    let world_wit = format!(
        r#"package mik:{name}@0.1.0;

world {name} {{
    // Export the handler
    export mik:core/handler@0.1.0;
}}
"#
    );
    fs::write(dir.join("wit/world.wit"), world_wit)?;

    // Core handler interface
    let core_wit = r#"package mik:core@0.1.0;

/// Minimal handler interface - all types inline.
interface handler {
    /// HTTP methods.
    enum method {
        %get,
        %post,
        %put,
        %patch,
        %delete,
        %head,
        %options,
    }

    /// HTTP request data from the bridge.
    record request-data {
        method: method,
        path: string,
        headers: list<tuple<string, string>>,
        body: option<list<u8>>,
    }

    /// HTTP response.
    record response {
        status: u16,
        headers: list<tuple<string, string>>,
        body: option<list<u8>>,
    }

    /// Process an HTTP request and return a response.
    handle: func(req: request-data) -> response;
}
"#;
    fs::write(dir.join("wit/deps/core/core.wit"), core_wit)?;

    Ok(())
}

/// Write .gitignore file.
fn write_gitignore(dir: &Path) -> Result<()> {
    let content = r#"/target
/modules
Cargo.lock
*.wasm
"#;
    fs::write(dir.join(".gitignore"), content)?;
    Ok(())
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
