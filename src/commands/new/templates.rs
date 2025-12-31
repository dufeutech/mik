//! Embedded templates for project scaffolding.
//!
//! Templates are embedded at compile-time using `include_str!()`.

use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Default version for new projects.
pub const DEFAULT_VERSION: &str = "0.1.0";

/// Supported programming languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    Rust,
    TypeScript,
}

impl std::str::FromStr for Language {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rust" | "rs" => Ok(Self::Rust),
            "typescript" | "ts" => Ok(Self::TypeScript),
            _ => Err(format!("unknown language: {s}")),
        }
    }
}

impl Language {
    /// Get available templates for this language.
    pub fn available_templates(&self) -> &[Template] {
        match self {
            Self::Rust => &[Template::Basic, Template::RestApi],
            Self::TypeScript => &[Template::Basic],
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rust => write!(f, "Rust"),
            Self::TypeScript => write!(f, "TypeScript"),
        }
    }
}

/// Available templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Template {
    #[default]
    Basic,
    RestApi,
}

impl std::str::FromStr for Template {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "rest-api" | "restapi" | "rest_api" => Ok(Self::RestApi),
            _ => Err(format!("unknown template: {s}")),
        }
    }
}

impl Template {
    /// Description of the template.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Basic => "Simple hello world handler",
            Self::RestApi => "CRUD REST API with typed inputs",
        }
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Basic => write!(f, "basic"),
            Self::RestApi => write!(f, "rest-api"),
        }
    }
}

/// Template context for variable substitution.
#[derive(Debug, Clone)]
pub struct TemplateContext {
    pub project_name: String,
    pub project_name_underscore: String,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub year: String,
    pub version: String,
}

impl TemplateContext {
    /// Replace template variables in content.
    pub fn render(&self, content: &str) -> String {
        content
            .replace("{{PROJECT_NAME}}", &self.project_name)
            .replace("{{PROJECT_NAME_UNDERSCORE}}", &self.project_name_underscore)
            .replace("{{AUTHOR_NAME}}", self.author_name.as_deref().unwrap_or(""))
            .replace(
                "{{AUTHOR_EMAIL}}",
                self.author_email.as_deref().unwrap_or(""),
            )
            .replace("{{YEAR}}", &self.year)
            .replace("{{VERSION}}", &self.version)
    }
}

/// Generate a project from embedded templates.
pub fn generate_project(
    dir: &Path,
    lang: Language,
    template: Template,
    ctx: &TemplateContext,
) -> Result<()> {
    match lang {
        Language::Rust => generate_rust_project(dir, template, ctx),
        Language::TypeScript => generate_typescript_project(dir, template, ctx),
    }
}

// ============================================================================
// Rust Templates
// ============================================================================

/// Create common project directories.
fn create_common_dirs(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir.join("src")).context("failed to create src directory")?;
    fs::create_dir_all(dir.join("wit/deps/core"))
        .context("failed to create wit/deps/core directory")?;
    Ok(())
}

fn generate_rust_project(dir: &Path, template: Template, ctx: &TemplateContext) -> Result<()> {
    // Create directories
    create_common_dirs(dir)?;
    fs::create_dir_all(dir.join("modules")).context("failed to create modules directory")?;

    // Cargo.toml
    let cargo_toml = ctx.render(RUST_CARGO_TOML);
    fs::write(dir.join("Cargo.toml"), &cargo_toml).context("failed to write Cargo.toml")?;

    // mik.toml
    let mik_toml = generate_mik_toml(ctx, Language::Rust)?;
    fs::write(dir.join("mik.toml"), &mik_toml).context("failed to write mik.toml")?;

    // src/lib.rs
    let lib_content = match template {
        Template::Basic => RUST_BASIC_LIB_RS,
        Template::RestApi => RUST_RESTAPI_LIB_RS,
    };
    let lib_rs = ctx.render(&format!("{RUST_LIB_HEADER}{lib_content}"));
    fs::write(dir.join("src/lib.rs"), &lib_rs).context("failed to write src/lib.rs")?;

    // WIT files
    let world_wit = ctx.render(RUST_WORLD_WIT);
    fs::write(dir.join("wit/world.wit"), &world_wit).context("failed to write wit/world.wit")?;
    fs::write(dir.join("wit/deps/core/core.wit"), CORE_WIT)
        .context("failed to write wit/deps/core/core.wit")?;

    // .gitignore
    let gitignore = format!("{RUST_GITIGNORE_EXTRA}{COMMON_GITIGNORE}");
    fs::write(dir.join(".gitignore"), &gitignore).context("failed to write .gitignore")?;

    // modules/.gitkeep
    fs::write(dir.join("modules/.gitkeep"), "").context("failed to write modules/.gitkeep")?;

    Ok(())
}

// ============================================================================
// TypeScript Templates
// ============================================================================

fn generate_typescript_project(
    dir: &Path,
    template: Template,
    ctx: &TemplateContext,
) -> Result<()> {
    // Currently only Basic template is supported for TypeScript
    // This match ensures consistency with generate_rust_project signature
    // and allows for future template expansion
    let _ = match template {
        Template::Basic => template,
        // RestApi and other templates not yet implemented for TypeScript
        _ => Template::Basic,
    };

    // Create directories
    create_common_dirs(dir)?;

    // mik.toml
    let mik_toml = generate_mik_toml(ctx, Language::TypeScript)?;
    fs::write(dir.join("mik.toml"), &mik_toml).context("failed to write mik.toml")?;

    // package.json
    let package_json = ctx.render(TS_PACKAGE_JSON);
    fs::write(dir.join("package.json"), &package_json).context("failed to write package.json")?;

    // tsconfig.json
    fs::write(dir.join("tsconfig.json"), TS_TSCONFIG).context("failed to write tsconfig.json")?;

    // src/component.ts
    let component_ts = ctx.render(TS_COMPONENT);
    fs::write(dir.join("src/component.ts"), &component_ts)
        .context("failed to write src/component.ts")?;

    // WIT files for mik:core/handler (embedded, no fetch needed)
    fs::write(dir.join("wit/handler.wit"), TS_HANDLER_WIT)
        .context("failed to write wit/handler.wit")?;
    fs::write(dir.join("wit/deps/core/core.wit"), CORE_WIT)
        .context("failed to write wit/deps/core/core.wit")?;

    // README.md
    let readme = ctx.render(TS_README);
    fs::write(dir.join("README.md"), &readme).context("failed to write README.md")?;

    // .gitignore
    let gitignore = format!("{TS_GITIGNORE_EXTRA}{COMMON_GITIGNORE}");
    fs::write(dir.join(".gitignore"), &gitignore).context("failed to write .gitignore")?;

    Ok(())
}

// ============================================================================
// mik.toml generation
// ============================================================================

fn generate_mik_toml(ctx: &TemplateContext, lang: Language) -> Result<String> {
    use crate::manifest::{Author, CompositionConfig, Manifest, Project, ServerConfig};

    let manifest = Manifest {
        project: Project {
            name: ctx.project_name.clone(),
            version: ctx.version.clone(),
            description: Some("A WASI HTTP component".to_string()),
            authors: ctx.author_name.as_ref().map_or(vec![], |name| {
                vec![Author {
                    name: name.clone(),
                    email: ctx.author_email.clone(),
                }]
            }),
            // Only set language for non-Rust projects (Rust is default)
            language: match lang {
                Language::Rust => None,
                Language::TypeScript => Some("typescript".to_string()),
            },
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

    toml::to_string_pretty(&manifest).context("failed to serialize mik.toml")
}

// ============================================================================
// Embedded Template Content
// ============================================================================

/// Common header for all Rust lib.rs templates.
/// Contains bindings import and mik-sdk prelude.
const RUST_LIB_HEADER: &str = r"#[allow(warnings)]
mod bindings;

use bindings::exports::mik::core::handler::{self, Guest, Response};
use mik_sdk::prelude::*;

";

// --- Rust Basic ---
const RUST_CARGO_TOML: &str = r#"[package]
name = "{{PROJECT_NAME}}"
version = "{{VERSION}}"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen-rt = "0.44.0"
mik-sdk = { git = "https://github.com/dufeut/mik-sdk", default-features = false }

[package.metadata.component]
package = "mik:{{PROJECT_NAME}}"

[package.metadata.component.target]
path = "wit"
world = "{{PROJECT_NAME}}"

[package.metadata.component.target.dependencies]
"mik:core" = { path = "wit/deps/core" }
"#;

const RUST_BASIC_LIB_RS: &str = r#"//! {{PROJECT_NAME}} - A mik HTTP handler

routes! {
    GET "/" | "" => home,
    GET "/health" => health,
}

fn home(_req: &Request) -> Response {
    ok!({
        "service": "{{PROJECT_NAME}}",
        "message": "Hello from mik!"
    })
}

fn health(_req: &Request) -> Response {
    ok!({ "status": "healthy" })
}
"#;

const RUST_RESTAPI_LIB_RS: &str = r#"//! {{PROJECT_NAME}} - A REST API built with mik
//!
//! Demonstrates: CRUD, Path params, Query strings, JSON bodies
//! Uses typed inputs in routes for automatic extraction

// ---- Types ----

/// Path parameter for item ID
#[derive(Path)]
struct ItemPath {
    id: String,
}

/// Query parameters for listing items
#[derive(Query)]
struct ListQuery {
    #[field(default = 1)]
    page: u32,
    #[field(default = 10)]
    limit: u32,
}

/// JSON body for creating/updating items
#[derive(Type)]
struct ItemInput {
    name: String,
    description: Option<String>,
}

/// Item response structure
#[derive(Type)]
struct Item {
    id: String,
    name: String,
    description: Option<String>,
}

// ---- Routes with typed inputs ----

routes! {
    GET "/health" => health,
    GET "/items" => list_items(query: ListQuery),
    GET "/items/{id}" => get_item(path: ItemPath),
    POST "/items" => create_item(body: ItemInput),
    PUT "/items/{id}" => update_item(path: ItemPath, body: ItemInput),
    DELETE "/items/{id}" => delete_item(path: ItemPath),
}

// ---- Handlers ----

fn health(_req: &Request) -> Response {
    ok!({ "status": "healthy" })
}

fn list_items(query: ListQuery, _req: &Request) -> Response {
    let items: Vec<Item> = vec![];

    ok!({
        "items": items,
        "page": query.page,
        "limit": query.limit,
        "total": 0
    })
}

fn get_item(path: ItemPath, _req: &Request) -> Response {
    let _ = path.id;
    not_found!("Item not found")
}

fn create_item(body: ItemInput, _req: &Request) -> Response {
    let id = random::uuid();

    created!("/items/{}", id, Item {
        id,
        name: body.name,
        description: body.description,
    })
}

fn update_item(path: ItemPath, body: ItemInput, _req: &Request) -> Response {
    ok!(Item {
        id: path.id,
        name: body.name,
        description: body.description,
    })
}

fn delete_item(path: ItemPath, _req: &Request) -> Response {
    let _ = path.id;
    no_content!()
}
"#;

const RUST_WORLD_WIT: &str = r"package mik:{{PROJECT_NAME}}@{{VERSION}};

world {{PROJECT_NAME}} {
    // Export the handler
    export mik:core/handler@0.1.0;
}
";

const CORE_WIT: &str = r"package mik:core@0.1.0;

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
";

/// Common gitignore entries shared across languages.
const COMMON_GITIGNORE: &str = "*.wasm\n";

const RUST_GITIGNORE_EXTRA: &str = r"/target
/modules
Cargo.lock
";

// --- TypeScript ---
const TS_PACKAGE_JSON: &str = r#"{
  "name": "{{PROJECT_NAME}}",
  "version": "1.0.0",
  "type": "module",
  "scripts": {
    "build:bundle": "esbuild src/component.ts --bundle --outfile=dist/component.js --format=esm --platform=neutral --external:mik:*",
    "build:wasm": "npx jco componentize dist/component.js --wit wit --world-name handler -o {{PROJECT_NAME}}.wasm",
    "build": "npm run build:bundle && npm run build:wasm",
    "clean": "rm -rf dist {{PROJECT_NAME}}.wasm"
  },
  "keywords": ["wasi", "wasm", "typescript", "mik"],
  "author": "{{AUTHOR_NAME}}",
  "license": "MIT",
  "description": "mik handler in TypeScript",
  "devDependencies": {
    "@bytecodealliance/componentize-js": "^0.19.3",
    "@bytecodealliance/jco": "^1.15.4",
    "esbuild": "^0.27.2",
    "typescript": "^5.9.3"
  }
}
"#;

const TS_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ES2022",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true,
    "outDir": "./dist",
    "rootDir": "./src"
  },
  "include": ["src/**/*"]
}
"#;

// WIT files for mik:core/handler (simple interface, composed with bridge)
const TS_HANDLER_WIT: &str = r"package mik:handler@0.1.0;

world handler {
    export mik:core/handler@0.1.0;
}
";

const TS_COMPONENT: &str = r#"// {{PROJECT_NAME}} - A mik handler in TypeScript
//
// Uses the simple mik:core/handler interface.
// The bridge component handles HTTP protocol details.

import { RequestData, Response, Method } from "mik:core/handler@0.1.0";

// Method enum to string
function methodToString(method: Method): string {
  const methods: Record<string, string> = {
    get: "GET",
    post: "POST",
    put: "PUT",
    patch: "PATCH",
    delete: "DELETE",
    head: "HEAD",
    options: "OPTIONS",
  };
  return methods[method] ?? "UNKNOWN";
}

// Export the handler interface
export const handler = {
  handle(req: RequestData): Response {
    // Build JSON response
    const body = JSON.stringify({
      message: "Hello from TypeScript!",
      service: "{{PROJECT_NAME}}",
      path: req.path,
      method: methodToString(req.method),
    });

    return {
      status: 200,
      headers: [["content-type", "application/json"]],
      body: new TextEncoder().encode(body),
    };
  },
};
"#;

const TS_README: &str = r"# {{PROJECT_NAME}}

A mik handler written in TypeScript using the simple `mik:core/handler` interface.

## Prerequisites

- Node.js 18+
- npm

## Build

```bash
mik build
```

Or manually:
```bash
npm install
npm run build
```

## Run

```bash
mik run
```

## Interface

This handler uses the simple `mik:core/handler` interface. The bridge component
handles HTTP protocol details, so your code just processes requests and returns responses.

## Documentation

See: https://dufeut.github.io/mik/guides/building-components/
";

const TS_GITIGNORE_EXTRA: &str = r"node_modules/
dist/
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_from_str() {
        assert_eq!("rust".parse::<Language>(), Ok(Language::Rust));
        assert_eq!("rs".parse::<Language>(), Ok(Language::Rust));
        assert_eq!("typescript".parse::<Language>(), Ok(Language::TypeScript));
        assert_eq!("ts".parse::<Language>(), Ok(Language::TypeScript));
        assert!("invalid".parse::<Language>().is_err());
    }

    #[test]
    fn test_template_from_str() {
        assert_eq!("basic".parse::<Template>(), Ok(Template::Basic));
        assert_eq!("rest-api".parse::<Template>(), Ok(Template::RestApi));
        assert_eq!("restapi".parse::<Template>(), Ok(Template::RestApi));
        assert!("invalid".parse::<Template>().is_err());
    }

    #[test]
    fn test_template_context_render() {
        let ctx = TemplateContext {
            project_name: "my-service".to_string(),
            project_name_underscore: "my_service".to_string(),
            author_name: Some("Test Author".to_string()),
            author_email: Some("test@example.com".to_string()),
            year: "2025".to_string(),
            version: DEFAULT_VERSION.to_string(),
        };

        let input = "name = \"{{PROJECT_NAME}}\"\nauthor = \"{{AUTHOR_NAME}}\"";
        let output = ctx.render(input);
        assert_eq!(output, "name = \"my-service\"\nauthor = \"Test Author\"");
    }

    #[test]
    fn test_available_templates() {
        assert_eq!(
            Language::Rust.available_templates(),
            &[Template::Basic, Template::RestApi]
        );
        assert_eq!(
            Language::TypeScript.available_templates(),
            &[Template::Basic]
        );
    }
}
