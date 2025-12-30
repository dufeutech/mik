//! Sync WASM components from registry.
//!
//! Downloads components from OCI registries or HTTP URLs.
//! Components are stored in `modules/` directory.
//!
//! OCI is the preferred method. HTTP is supported as fallback.

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use oci_client::{Client, Reference};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

use crate::manifest::{Dependency, Manifest};
use crate::registry;

/// Pull a single dependency.
async fn pull_dependency(name: &str, dep: &Dependency) -> Result<String> {
    let output_path = PathBuf::from(format!("modules/{name}.wasm"));

    match dep {
        Dependency::Simple(version) => {
            // Simple version string - default to ghcr.io registry
            // Name must include namespace (e.g., "user/repo")
            if name.contains('/') {
                let oci_ref = format!("ghcr.io/{name}:{version}");
                pull_oci(&oci_ref, &output_path).await?;
            } else {
                anyhow::bail!(
                    "Dependency '{name}' requires namespace (e.g., 'user/{name}').\n\
                     Use: \"{name}\" = {{ registry = \"ghcr.io/user/{name}:latest\" }}"
                );
            }
        },
        Dependency::Detailed(d) => {
            if let Some(path) = &d.path {
                // Local path - just copy (check extension case-insensitively)
                if Path::new(path)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("wasm"))
                {
                    fs::copy(path, &output_path)?;
                } else {
                    // It's a project directory, look for built wasm
                    let wasm = format!(
                        "{}/target/wasm32-wasip2/release/{}.wasm",
                        path,
                        name.replace('-', "_")
                    );
                    if Path::new(&wasm).exists() {
                        fs::copy(&wasm, &output_path)?;
                    } else {
                        anyhow::bail!("Not found: {wasm}");
                    }
                }
            } else if let Some(reg) = &d.registry {
                // Registry - OCI or HTTP URL
                if is_oci_reference(reg) {
                    pull_oci(reg, &output_path).await?;
                } else {
                    // HTTP URL fallback
                    registry::download(reg, &output_path)?;
                }
            } else if let Some(git_url) = &d.git {
                // Git dependency - clone/fetch and extract wasm
                let git_ref = GitRef::from_detail(d);
                pull_git_dependency(git_url, &git_ref, name, &output_path.to_string_lossy())?;
            } else {
                anyhow::bail!("No source specified for dependency");
            }
        },
    }

    Ok(output_path.to_string_lossy().to_string())
}

/// Check if a registry reference is an OCI reference (not HTTP URL).
fn is_oci_reference(registry: &str) -> bool {
    !registry.starts_with("http://") && !registry.starts_with("https://")
}

/// Pull from OCI registry using oci-client.
pub async fn pull_oci(oci_ref: &str, output_path: &Path) -> Result<()> {
    // Parse the OCI reference
    let reference: Reference = oci_ref
        .parse()
        .with_context(|| format!("Invalid OCI reference: {oci_ref}"))?;

    // Create parent directory
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Create OCI client with default config (anonymous access)
    let client = Client::default();

    // Try to authenticate (for public registries this gets anonymous token)
    let auth = oci_client::secrets::RegistryAuth::Anonymous;

    // Pull the manifest to get layer info
    let (manifest, _digest) = client
        .pull_manifest(&reference, &auth)
        .await
        .with_context(|| format!("Failed to pull manifest for {oci_ref}"))?;

    // Get the first layer (should be the .wasm file)
    let layers = match &manifest {
        oci_client::manifest::OciManifest::Image(img) => &img.layers,
        oci_client::manifest::OciManifest::ImageIndex(_) => {
            anyhow::bail!("Image index not supported, expected single image");
        },
    };

    if layers.is_empty() {
        anyhow::bail!("No layers found in manifest for {oci_ref}");
    }

    // Find the wasm layer (usually application/wasm or application/vnd.wasm.content.layer.v1+wasm)
    let wasm_layer = layers
        .iter()
        .find(|l| {
            l.media_type.contains("wasm")
                || l.media_type.contains("octet-stream")
                || l.media_type.is_empty()
        })
        .or_else(|| layers.first())
        .ok_or_else(|| anyhow::anyhow!("No suitable layer found in {oci_ref}"))?;

    // Pull the blob
    let mut file = File::create(output_path)
        .await
        .with_context(|| format!("Failed to create {}", output_path.display()))?;

    client
        .pull_blob(&reference, wasm_layer, &mut file)
        .await
        .with_context(|| format!("Failed to pull blob from {oci_ref}"))?;

    file.flush().await?;

    Ok(())
}

/// Sync all dependencies (pull missing, remove stale).
pub async fn sync() -> Result<()> {
    let Ok(manifest) = Manifest::load() else {
        println!("No mik.toml found, nothing to sync");
        return Ok(());
    };

    // Collect expected module names
    let expected: HashSet<String> = manifest
        .dependencies
        .keys()
        .chain(manifest.dev_dependencies.keys())
        .cloned()
        .collect();

    // Remove stale modules
    remove_stale_modules(&expected);

    // Find missing dependencies
    let missing: Vec<_> = manifest
        .dependencies
        .iter()
        .filter(|(_, dep)| !is_local_path_dependency(dep))
        .filter(|(name, _)| !Path::new(&format!("modules/{name}.wasm")).exists())
        .map(|(name, dep)| (name.clone(), dep.clone()))
        .collect();

    if missing.is_empty() {
        println!("All dependencies up to date");
        return Ok(());
    }

    println!("Pulling {} missing dependencies...", missing.len());
    fs::create_dir_all("modules")?;

    let progress = ProgressBar::new(missing.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .map(|s| s.progress_chars("=> "))
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );

    for (name, dep) in &missing {
        progress.set_message(format!("Pulling {name}"));
        match pull_dependency(name, dep).await {
            Ok(path) => println!("  + {name}: {path}"),
            Err(e) => println!("  ! {name}: {e}"),
        }
        progress.inc(1);
    }

    progress.finish_and_clear();

    Ok(())
}

/// Remove modules not in the expected set.
fn remove_stale_modules(expected: &HashSet<String>) {
    let modules_dir = Path::new("modules");
    if !modules_dir.is_dir() {
        return;
    }

    let Ok(entries) = fs::read_dir(modules_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "wasm") {
            continue;
        }

        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        if !expected.contains(stem) && fs::remove_file(&path).is_ok() {
            println!("  - removed stale: {}", path.display());
        }
    }
}

/// Check if dependency is a local path reference.
fn is_local_path_dependency(dep: &Dependency) -> bool {
    matches!(dep, Dependency::Detailed(d) if d.path.is_some())
}

/// Git reference type (branch, tag, or commit).
#[derive(Debug)]
enum GitRef {
    Branch(String),
    Tag(String),
    Rev(String),
    Default,
}

impl GitRef {
    /// Extract git ref from dependency detail.
    fn from_detail(detail: &crate::manifest::DependencyDetail) -> Self {
        if let Some(branch) = &detail.branch {
            GitRef::Branch(branch.clone())
        } else if let Some(tag) = &detail.tag {
            GitRef::Tag(tag.clone())
        } else if let Some(rev) = &detail.rev {
            GitRef::Rev(rev.clone())
        } else {
            GitRef::Default
        }
    }
}

/// Pull a dependency from a git repository.
fn pull_git_dependency(
    git_url: &str,
    git_ref: &GitRef,
    name: &str,
    output_path: &str,
) -> Result<()> {
    // Get cache directory for git repositories
    let cache_dir = get_git_cache_dir()?;

    // Create a deterministic directory name from the git URL
    let repo_cache_dir = cache_dir.join(sanitize_url_for_path(git_url));

    // Clone or update the repository
    let repo = if repo_cache_dir.exists() {
        // Repository already exists, open and fetch updates
        let repo = git2::Repository::open(&repo_cache_dir)
            .context("Failed to open cached git repository")?;

        // Fetch updates from remote
        fetch_updates(&repo)?;
        repo
    } else {
        // Clone fresh repository
        fs::create_dir_all(&cache_dir)?;
        git2::Repository::clone(git_url, &repo_cache_dir)
            .with_context(|| format!("Failed to clone git repository: {git_url}"))?
    };

    // Checkout the specified ref
    checkout_ref(&repo, git_ref)?;

    // Find .wasm file in the repository
    let wasm_path = find_wasm_file(&repo_cache_dir, name)?;

    // Copy to output location
    fs::copy(&wasm_path, output_path)
        .with_context(|| format!("Failed to copy {} to {}", wasm_path.display(), output_path))?;

    Ok(())
}

/// Get the cache directory for git repositories.
fn get_git_cache_dir() -> Result<PathBuf> {
    let cache_dir = dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("Failed to determine cache directory"))?
        .join("mik")
        .join("git");
    Ok(cache_dir)
}

/// Sanitize a URL to create a safe directory name.
fn sanitize_url_for_path(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Create a hash of the URL for uniqueness
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();

    // Extract a readable name from the URL if possible
    let name = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or("repo");

    // Combine readable name with hash for uniqueness
    format!("{name}-{hash:x}")
}

/// Fetch updates from the remote repository.
fn fetch_updates(repo: &git2::Repository) -> Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .context("Failed to find 'origin' remote")?;

    remote
        .fetch(&["refs/heads/*:refs/heads/*"], None, None)
        .context("Failed to fetch updates from remote")?;

    Ok(())
}

/// Checkout a specific git reference.
fn checkout_ref(repo: &git2::Repository, git_ref: &GitRef) -> Result<()> {
    let (object, reference) = match git_ref {
        GitRef::Branch(branch) => {
            // Try remote branch first, then local
            let refname = format!("refs/heads/{branch}");
            let obj = repo
                .revparse_single(&refname)
                .or_else(|_| repo.revparse_single(&format!("origin/{branch}")))
                .with_context(|| format!("Branch '{branch}' not found"))?;
            (obj, refname)
        },
        GitRef::Tag(tag) => {
            let refname = format!("refs/tags/{tag}");
            let obj = repo
                .revparse_single(&refname)
                .with_context(|| format!("Tag '{tag}' not found"))?;
            (obj, refname)
        },
        GitRef::Rev(rev) => {
            let obj = repo
                .revparse_single(rev)
                .with_context(|| format!("Revision '{rev}' not found"))?;
            (obj, rev.clone())
        },
        GitRef::Default => {
            // Use default branch (usually main/master)
            let head = repo.head().context("Failed to get HEAD")?;
            let obj = head.peel(git2::ObjectType::Commit)?;
            (obj, "HEAD".to_string())
        },
    };

    repo.checkout_tree(&object, None)
        .with_context(|| format!("Failed to checkout {reference}"))?;

    repo.set_head_detached(object.id())
        .context("Failed to set HEAD")?;

    Ok(())
}

/// Find a .wasm file in the repository.
///
/// Search order:
/// 1. Look for `{name}.wasm` in the root
/// 2. Look for `target/wasm32-wasip2/release/{name}.wasm` (built artifact)
/// 3. Look for any .wasm file in common locations
fn find_wasm_file(repo_dir: &Path, name: &str) -> Result<PathBuf> {
    // Strategy 1: Look for {name}.wasm in root
    let direct_path = repo_dir.join(format!("{name}.wasm"));
    if direct_path.exists() {
        return Ok(direct_path);
    }

    // Strategy 2: Look in build output directory
    let name_underscore = name.replace('-', "_");
    let build_paths = [
        repo_dir.join(format!(
            "target/wasm32-wasip2/release/{name_underscore}.wasm"
        )),
        repo_dir.join(format!("target/wasm32-wasi/release/{name_underscore}.wasm")),
        repo_dir.join(format!("target/wasm32-wasip2/release/{name}.wasm")),
        repo_dir.join(format!("target/wasm32-wasi/release/{name}.wasm")),
    ];

    for path in &build_paths {
        if path.exists() {
            return Ok(path.clone());
        }
    }

    // Strategy 3: Search for any .wasm file
    let wasm_files = find_wasm_files_recursive(repo_dir)?;

    match wasm_files.len() {
        0 => anyhow::bail!(
            "No .wasm file found in repository.\n  \
             Expected:\n  \
             - {name}.wasm in repository root\n  \
             - target/wasm32-wasip2/release/{name_underscore}.wasm (built artifact)\n  \
             Please build the project or ensure the .wasm file is committed."
        ),
        1 => Ok(wasm_files[0].clone()),
        _ => {
            // Multiple .wasm files found, try to pick the most relevant one
            let preferred = wasm_files
                .iter()
                .find(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        n == format!("{name}.wasm") || n == format!("{name_underscore}.wasm")
                    })
                })
                .or_else(|| wasm_files.first());

            if let Some(path) = preferred {
                Ok(path.clone())
            } else {
                anyhow::bail!(
                    "Multiple .wasm files found in repository:\n{}\n  \
                     Please specify which file to use by committing only the desired .wasm file.",
                    wasm_files
                        .iter()
                        .map(|p| format!("  - {}", p.display()))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
        },
    }
}

/// Recursively find all .wasm files in a directory, excluding build artifacts dirs.
fn find_wasm_files_recursive(dir: &Path) -> Result<Vec<PathBuf>> {
    fn walk_dir(dir: &Path, skip_dirs: &[&str], wasm_files: &mut Vec<PathBuf>) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                if !skip_dirs.contains(&dir_name) {
                    walk_dir(&path, skip_dirs, wasm_files)?;
                }
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wasm"))
            {
                wasm_files.push(path);
            }
        }

        Ok(())
    }

    let mut wasm_files = Vec::new();

    // Directories to skip (build artifacts, dependencies, etc.)
    let skip_dirs = ["target", "node_modules", ".git", "deps"];

    walk_dir(dir, &skip_dirs, &mut wasm_files)?;
    Ok(wasm_files)
}
