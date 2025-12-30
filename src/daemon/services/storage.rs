//! Embedded S3-like object storage service for the mik daemon.
//!
//! Provides filesystem-based object storage with metadata tracking using redb.
//! Objects are stored in `~/.mik/storage/` with metadata in a companion database.
//!
//! Security features:
//! - Path traversal protection (prevents escaping storage directory)
//! - Atomic operations with ACID guarantees via redb
//! - Content-type validation and storage
//!
//! # Async Usage
//!
//! All database operations are blocking. When using from async contexts,
//! use the async methods (`get_object_async`, `put_object_async`, etc.)
//! which automatically wrap operations in `spawn_blocking` to avoid blocking
//! the async runtime.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Table for object metadata storage
const OBJECTS_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("objects");

/// Metadata for a stored object
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectMeta {
    /// Virtual path of the object (e.g., "images/logo.png")
    pub path: String,
    /// Size in bytes
    pub size: u64,
    /// MIME content type (e.g., "image/png", "application/json")
    pub content_type: String,
    /// Timestamp when object was created
    pub created_at: DateTime<Utc>,
    /// Timestamp when object was last modified
    pub modified_at: DateTime<Utc>,
}

/// Embedded object storage service.
///
/// Stores objects on the filesystem with metadata tracked in redb for
/// fast queries and listing operations.
///
/// # Thread Safety
///
/// `StorageService` is `Clone` and can be shared across threads. The underlying
/// database handles concurrent access safely.
#[derive(Clone)]
pub struct StorageService {
    /// Base directory for object storage (e.g., ~/.mik/storage)
    base_dir: PathBuf,
    /// Metadata database
    db: Arc<Database>,
}

impl StorageService {
    /// Creates or opens the storage service at the given base directory.
    ///
    /// # Arguments
    /// * `base_dir` - Root directory for object storage (e.g., ~/.mik/storage)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Storage directory cannot be created
    /// - Metadata database cannot be opened or initialized
    /// - Metadata reconciliation fails (filesystem scan errors)
    ///
    /// # Security
    /// All object paths are normalized and validated to prevent directory
    /// traversal attacks. Paths containing `..`, absolute paths, or other
    /// suspicious components are rejected.
    pub fn open<P: AsRef<Path>>(base_dir: P) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();

        // Create base directory if it doesn't exist
        fs::create_dir_all(&base_dir).with_context(|| {
            format!("Failed to create storage directory: {}", base_dir.display())
        })?;

        // Create metadata database in base directory
        let db_path = base_dir.join("metadata.redb");
        let db = Database::create(&db_path).with_context(|| {
            format!(
                "Failed to open storage metadata database: {}",
                db_path.display()
            )
        })?;

        // Initialize metadata table
        let write_txn = db
            .begin_write()
            .context("Failed to begin initialization transaction")?;
        {
            let _table = write_txn
                .open_table(OBJECTS_TABLE)
                .context("Failed to initialize objects table")?;
        }
        write_txn
            .commit()
            .context("Failed to commit initialization transaction")?;

        let service = Self {
            base_dir,
            db: Arc::new(db),
        };

        // Reconcile metadata with filesystem on startup
        service.reconcile()?;

        Ok(service)
    }

    /// Reconciles metadata database with actual filesystem state.
    ///
    /// This method is called automatically on startup to handle:
    /// - Files deleted outside the service (removes orphaned metadata)
    /// - Files added outside the service (creates missing metadata)
    /// - Files modified outside the service (updates stale metadata)
    ///
    /// # Errors
    ///
    /// Returns an error if directory scanning fails or database operations fail.
    ///
    /// # Performance
    /// This scans the entire storage directory and metadata table, so it
    /// may take time for large storage systems. Consider calling only
    /// on startup or periodically during low-traffic windows.
    pub fn reconcile(&self) -> Result<()> {
        tracing::debug!(base_dir = %self.base_dir.display(), "Reconciling storage metadata");

        // Phase 1: Collect all files from filesystem
        let mut fs_files: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.scan_directory(&self.base_dir, &mut fs_files)?;

        // Phase 2: Check metadata entries against filesystem
        let mut orphaned_entries: Vec<String> = Vec::new();
        let mut stale_entries: Vec<(String, u64)> = Vec::new(); // (path, actual_size)

        {
            let read_txn = self
                .db
                .begin_read()
                .context("Failed to begin read transaction for reconciliation")?;
            let table = read_txn
                .open_table(OBJECTS_TABLE)
                .context("Failed to open objects table for reconciliation")?;

            for item in table.iter().context("Failed to iterate objects table")? {
                let (key, value) = item.context("Failed to read object entry")?;
                let path = key.value().to_string();

                if fs_files.contains(&path) {
                    // File exists, check if metadata is stale
                    fs_files.remove(&path); // Mark as seen

                    if let Ok(meta) = serde_json::from_slice::<ObjectMeta>(value.value())
                        && let Ok(file_meta) = fs::metadata(self.base_dir.join(&path))
                        && file_meta.len() != meta.size
                    {
                        stale_entries.push((path, file_meta.len()));
                    }
                } else {
                    // Metadata exists but file is gone
                    orphaned_entries.push(path);
                }
            }
        }

        // Phase 3: Remove orphaned metadata entries
        if !orphaned_entries.is_empty() {
            tracing::info!(
                count = orphaned_entries.len(),
                "Removing orphaned metadata entries"
            );
            for path in &orphaned_entries {
                self.remove_metadata(path)?;
            }
        }

        // Phase 4: Add missing metadata entries (files without metadata)
        if !fs_files.is_empty() {
            tracing::info!(
                count = fs_files.len(),
                "Creating metadata for untracked files"
            );
            for path in &fs_files {
                let file_path = self.base_dir.join(path);
                if let Ok(file_meta) = fs::metadata(&file_path) {
                    let content_type = mime_guess::from_path(&file_path)
                        .first()
                        .map_or_else(|| "application/octet-stream".to_string(), |m| m.to_string());

                    let now = Utc::now();
                    let meta = ObjectMeta {
                        path: path.clone(),
                        size: file_meta.len(),
                        content_type,
                        created_at: now,
                        modified_at: now,
                    };
                    self.save_metadata(&meta)?;
                }
            }
        }

        // Phase 5: Update stale metadata entries
        if !stale_entries.is_empty() {
            tracing::info!(
                count = stale_entries.len(),
                "Updating stale metadata entries"
            );
            for (path, actual_size) in &stale_entries {
                if let Some(mut meta) = self.load_metadata(path)? {
                    meta.size = *actual_size;
                    meta.modified_at = Utc::now();
                    self.save_metadata(&meta)?;
                }
            }
        }

        let total_fixes = orphaned_entries.len() + fs_files.len() + stale_entries.len();
        if total_fixes > 0 {
            tracing::info!(
                orphaned = orphaned_entries.len(),
                untracked = fs_files.len(),
                stale = stale_entries.len(),
                "Storage reconciliation complete"
            );
        } else {
            tracing::debug!("Storage metadata is consistent with filesystem");
        }

        Ok(())
    }

    /// Recursively scans a directory and collects relative paths to files.
    fn scan_directory(
        &self,
        dir: &Path,
        files: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        if !dir.exists() || !dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)
            .with_context(|| format!("Failed to read directory: {}", dir.display()))?
        {
            let entry = entry.context("Failed to read directory entry")?;
            let path = entry.path();

            // Skip the metadata database file
            if path.file_name().is_some_and(|n| n == "metadata.redb") {
                continue;
            }
            // Skip redb lock files
            if path.extension().is_some_and(|e| e == "lock") {
                continue;
            }

            if path.is_dir() {
                self.scan_directory(&path, files)?;
            } else if path.is_file() {
                // Convert to relative path from base_dir
                if let Ok(relative) = path.strip_prefix(&self.base_dir) {
                    // Normalize path separators for cross-platform consistency
                    let relative_str = relative.to_string_lossy().replace('\\', "/");
                    files.insert(relative_str);
                }
            }
        }

        Ok(())
    }

    /// Validates and normalizes an object path to prevent directory traversal.
    ///
    /// # Security
    /// Rejects paths that:
    /// - Are absolute (start with `/` or drive letter)
    /// - Contain `..` components
    /// - Contain special components like root or prefix
    /// - Are empty
    ///
    /// # Examples
    /// ```
    /// // Valid paths
    /// validate_path("images/logo.png")     // Ok("images/logo.png")
    /// validate_path("./data/file.json")    // Ok("data/file.json")
    ///
    /// // Invalid paths
    /// validate_path("../etc/passwd")       // Error: path traversal
    /// validate_path("/etc/passwd")         // Error: absolute path
    /// validate_path("")                    // Error: empty path
    /// ```
    #[allow(clippy::unused_self)]
    fn validate_path(&self, path: &str) -> Result<PathBuf> {
        if path.is_empty() {
            bail!("Object path cannot be empty");
        }

        let path = Path::new(path);

        // Reject absolute paths
        if path.is_absolute() {
            bail!("Object path cannot be absolute: {}", path.display());
        }

        // Normalize and check for path traversal attempts
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(name) => normalized.push(name),
                Component::CurDir => {}, // Skip "." components
                Component::ParentDir => {
                    bail!("Object path cannot contain '..': {}", path.display())
                },
                Component::RootDir | Component::Prefix(_) => {
                    bail!(
                        "Object path cannot contain root or prefix: {}",
                        path.display()
                    )
                },
            }
        }

        if normalized.as_os_str().is_empty() {
            bail!("Object path normalized to empty path");
        }

        Ok(normalized)
    }

    /// Returns the filesystem path for an object.
    fn object_path(&self, path: &str) -> Result<PathBuf> {
        let normalized = self.validate_path(path)?;
        Ok(self.base_dir.join(normalized))
    }

    /// Stores an object with metadata.
    ///
    /// # Arguments
    /// * `path` - Virtual path for the object (e.g., "images/logo.png")
    /// * `data` - Object data bytes
    /// * `content_type` - Optional MIME type (auto-detected if None)
    ///
    /// # Returns
    /// Metadata of the stored object
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Path is invalid (empty, absolute, or contains `..`)
    /// - Parent directories cannot be created
    /// - File cannot be written (permissions, disk full)
    /// - Metadata cannot be saved to database
    ///
    /// # Example
    /// ```no_run
    /// let storage = StorageService::open("~/.mik/storage")?;
    /// let meta = storage.put_object(
    ///     "images/logo.png",
    ///     &image_bytes,
    ///     Some("image/png")
    /// )?;
    /// println!("Stored {} bytes", meta.size);
    /// ```
    pub fn put_object(
        &self,
        path: &str,
        data: &[u8],
        content_type: Option<&str>,
    ) -> Result<ObjectMeta> {
        let file_path = self.object_path(path)?;

        // Create parent directories if needed
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directories for: {path}"))?;
        }

        // Write object data to filesystem
        fs::write(&file_path, data).with_context(|| format!("Failed to write object: {path}"))?;

        // Determine content type
        let content_type = content_type
            .map(std::string::ToString::to_string)
            .or_else(|| {
                // Auto-detect from file extension
                mime_guess::from_path(&file_path)
                    .first()
                    .map(|mime| mime.to_string())
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());

        // Create metadata
        let now = Utc::now();
        let meta = ObjectMeta {
            path: path.to_string(),
            size: data.len() as u64,
            content_type,
            created_at: now,
            modified_at: now,
        };

        // Store metadata in database
        self.save_metadata(&meta)?;

        Ok(meta)
    }

    /// Retrieves an object and its metadata.
    ///
    /// # Returns
    /// * `Ok(Some((data, meta)))` - Object found
    /// * `Ok(None)` - Object not found
    ///
    /// # Errors
    ///
    /// Returns an error if the path is invalid or the file cannot be read.
    ///
    /// # Example
    /// ```no_run
    /// if let Some((data, meta)) = storage.get_object("images/logo.png")? {
    ///     println!("Content-Type: {}", meta.content_type);
    ///     println!("Size: {} bytes", data.len());
    /// }
    /// ```
    pub fn get_object(&self, path: &str) -> Result<Option<(Vec<u8>, ObjectMeta)>> {
        let file_path = self.object_path(path)?;

        // Check if file exists
        if !file_path.exists() {
            return Ok(None);
        }

        // Read object data
        let data =
            fs::read(&file_path).with_context(|| format!("Failed to read object: {path}"))?;

        // Load metadata
        let meta = if let Some(meta) = self.load_metadata(path)? {
            meta
        } else {
            // File exists but no metadata - reconstruct from filesystem
            let metadata = fs::metadata(&file_path)
                .with_context(|| format!("Failed to get file metadata: {path}"))?;

            let content_type = mime_guess::from_path(&file_path).first().map_or_else(
                || "application/octet-stream".to_string(),
                |mime| mime.to_string(),
            );

            let now = Utc::now();
            ObjectMeta {
                path: path.to_string(),
                size: metadata.len(),
                content_type,
                created_at: now,
                modified_at: now,
            }
        };

        Ok(Some((data, meta)))
    }

    /// Deletes an object and its metadata.
    ///
    /// # Returns
    /// * `Ok(true)` - Object existed and was deleted
    /// * `Ok(false)` - Object did not exist
    ///
    /// # Errors
    ///
    /// Returns an error if the path is invalid, file deletion fails, or
    /// metadata cannot be removed from the database.
    ///
    /// # Example
    /// ```no_run
    /// if storage.delete_object("images/old-logo.png")? {
    ///     println!("Object deleted");
    /// } else {
    ///     println!("Object not found");
    /// }
    /// ```
    pub fn delete_object(&self, path: &str) -> Result<bool> {
        let file_path = self.object_path(path)?;

        // Check if file exists
        if !file_path.exists() {
            // Also remove metadata if it exists (cleanup orphaned entries)
            self.remove_metadata(path)?;
            return Ok(false);
        }

        // Delete file
        fs::remove_file(&file_path).with_context(|| format!("Failed to delete object: {path}"))?;

        // Delete metadata
        self.remove_metadata(path)?;

        Ok(true)
    }

    /// Retrieves object metadata without downloading the object.
    ///
    /// # Returns
    /// * `Ok(Some(meta))` - Object found
    /// * `Ok(None)` - Object not found
    ///
    /// # Errors
    ///
    /// Returns an error if the path is invalid or filesystem metadata cannot be read.
    ///
    /// # Example
    /// ```no_run
    /// if let Some(meta) = storage.head_object("images/logo.png")? {
    ///     println!("Size: {} bytes", meta.size);
    ///     println!("Modified: {}", meta.modified_at);
    /// }
    /// ```
    pub fn head_object(&self, path: &str) -> Result<Option<ObjectMeta>> {
        let file_path = self.object_path(path)?;

        // Check if file exists
        if !file_path.exists() {
            return Ok(None);
        }

        // Try to load metadata from database
        if let Some(meta) = self.load_metadata(path)? {
            return Ok(Some(meta));
        }

        // File exists but no metadata - reconstruct from filesystem
        let metadata = fs::metadata(&file_path)
            .with_context(|| format!("Failed to get file metadata: {path}"))?;

        let content_type = mime_guess::from_path(&file_path).first().map_or_else(
            || "application/octet-stream".to_string(),
            |mime| mime.to_string(),
        );

        let now = Utc::now();
        Ok(Some(ObjectMeta {
            path: path.to_string(),
            size: metadata.len(),
            content_type,
            created_at: now,
            modified_at: now,
        }))
    }

    /// Lists all objects, optionally filtered by prefix.
    ///
    /// # Arguments
    /// * `prefix` - Optional path prefix filter (e.g., "images/" lists only images)
    ///
    /// # Returns
    /// Vector of object metadata sorted by path
    ///
    /// # Errors
    ///
    /// Returns an error if the database read transaction fails or iteration errors occur.
    ///
    /// # Example
    /// ```no_run
    /// // List all objects
    /// let all_objects = storage.list_objects(None)?;
    ///
    /// // List only images
    /// let images = storage.list_objects(Some("images/"))?;
    /// ```
    pub fn list_objects(&self, prefix: Option<&str>) -> Result<Vec<ObjectMeta>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(OBJECTS_TABLE)
            .context("Failed to open objects table")?;

        let mut objects = Vec::new();

        for item in table.iter().context("Failed to iterate objects table")? {
            let (key, value) = item.context("Failed to read object entry")?;

            // Apply prefix filter
            if let Some(prefix) = prefix
                && !key.value().starts_with(prefix)
            {
                continue;
            }

            // Deserialize metadata
            if let Ok(meta) = serde_json::from_slice::<ObjectMeta>(value.value()) {
                objects.push(meta);
            }
        }

        // Sort by path for consistent ordering
        objects.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(objects)
    }

    /// Saves object metadata to the database.
    fn save_metadata(&self, meta: &ObjectMeta) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        {
            let mut table = write_txn
                .open_table(OBJECTS_TABLE)
                .context("Failed to open objects table")?;

            let json = serde_json::to_vec(meta).context("Failed to serialize object metadata")?;

            table
                .insert(meta.path.as_str(), json.as_slice())
                .with_context(|| format!("Failed to insert object metadata: {}", meta.path))?;
        }

        write_txn
            .commit()
            .context("Failed to commit metadata save transaction")?;

        Ok(())
    }

    /// Loads object metadata from the database.
    fn load_metadata(&self, path: &str) -> Result<Option<ObjectMeta>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(OBJECTS_TABLE)
            .context("Failed to open objects table")?;

        let result = table
            .get(path)
            .with_context(|| format!("Failed to read object metadata: {path}"))?;

        match result {
            Some(guard) => {
                let meta = serde_json::from_slice(guard.value())
                    .with_context(|| format!("Failed to deserialize object metadata: {path}"))?;
                Ok(Some(meta))
            },
            None => Ok(None),
        }
    }

    /// Removes object metadata from the database.
    fn remove_metadata(&self, path: &str) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        {
            let mut table = write_txn
                .open_table(OBJECTS_TABLE)
                .context("Failed to open objects table")?;

            table
                .remove(path)
                .with_context(|| format!("Failed to remove object metadata: {path}"))?;
        }

        write_txn
            .commit()
            .context("Failed to commit metadata removal transaction")?;

        Ok(())
    }

    // ========================================================================
    // Async Methods
    //
    // These methods wrap the synchronous operations in `spawn_blocking` to
    // avoid blocking the async runtime. Use these when calling from async
    // contexts (HTTP handlers, etc.).
    // ========================================================================

    /// Stores an object asynchronously.
    ///
    /// Async version of `put_object` that uses `spawn_blocking`.
    pub async fn put_object_async(
        &self,
        path: String,
        data: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<ObjectMeta> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || {
            service.put_object(&path, &data, content_type.as_deref())
        })
        .await
        .context("Task join error")?
    }

    /// Retrieves an object asynchronously.
    ///
    /// Async version of `get_object` that uses `spawn_blocking`.
    pub async fn get_object_async(&self, path: String) -> Result<Option<(Vec<u8>, ObjectMeta)>> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.get_object(&path))
            .await
            .context("Task join error")?
    }

    /// Deletes an object asynchronously.
    ///
    /// Async version of `delete_object` that uses `spawn_blocking`.
    pub async fn delete_object_async(&self, path: String) -> Result<bool> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.delete_object(&path))
            .await
            .context("Task join error")?
    }

    /// Retrieves object metadata asynchronously.
    ///
    /// Async version of `head_object` that uses `spawn_blocking`.
    pub async fn head_object_async(&self, path: String) -> Result<Option<ObjectMeta>> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.head_object(&path))
            .await
            .context("Task join error")?
    }

    /// Lists objects asynchronously.
    ///
    /// Async version of `list_objects` that uses `spawn_blocking`.
    pub async fn list_objects_async(&self, prefix: Option<String>) -> Result<Vec<ObjectMeta>> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.list_objects(prefix.as_deref()))
            .await
            .context("Task join error")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_storage() -> (StorageService, TempDir) {
        let tmp = TempDir::new().unwrap();
        let storage = StorageService::open(tmp.path()).unwrap();
        (storage, tmp)
    }

    #[test]
    fn test_put_and_get_object() {
        let (storage, _tmp) = create_storage();

        let data = b"Hello, World!";
        let meta = storage
            .put_object("test.txt", data, Some("text/plain"))
            .unwrap();

        assert_eq!(meta.path, "test.txt");
        assert_eq!(meta.size, 13);
        assert_eq!(meta.content_type, "text/plain");

        let (retrieved_data, retrieved_meta) = storage.get_object("test.txt").unwrap().unwrap();
        assert_eq!(retrieved_data, data);
        assert_eq!(retrieved_meta.path, "test.txt");
        assert_eq!(retrieved_meta.size, 13);
    }

    #[test]
    fn test_get_nonexistent_object() {
        let (storage, _tmp) = create_storage();

        let result = storage.get_object("nonexistent.txt").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_delete_object() {
        let (storage, _tmp) = create_storage();

        storage.put_object("test.txt", b"data", None).unwrap();

        let deleted = storage.delete_object("test.txt").unwrap();
        assert!(deleted);

        let result = storage.get_object("test.txt").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_delete_nonexistent_object() {
        let (storage, _tmp) = create_storage();

        let deleted = storage.delete_object("nonexistent.txt").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn test_head_object() {
        let (storage, _tmp) = create_storage();

        storage
            .put_object("test.txt", b"Hello", Some("text/plain"))
            .unwrap();

        let meta = storage.head_object("test.txt").unwrap().unwrap();
        assert_eq!(meta.path, "test.txt");
        assert_eq!(meta.size, 5);
        assert_eq!(meta.content_type, "text/plain");
    }

    #[test]
    fn test_head_nonexistent_object() {
        let (storage, _tmp) = create_storage();

        let result = storage.head_object("nonexistent.txt").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_objects() {
        let (storage, _tmp) = create_storage();

        storage
            .put_object("images/logo.png", b"png", Some("image/png"))
            .unwrap();
        storage
            .put_object("images/banner.jpg", b"jpg", Some("image/jpeg"))
            .unwrap();
        storage
            .put_object("docs/readme.md", b"md", Some("text/markdown"))
            .unwrap();

        let all_objects = storage.list_objects(None).unwrap();
        assert_eq!(all_objects.len(), 3);

        let images = storage.list_objects(Some("images/")).unwrap();
        assert_eq!(images.len(), 2);
        assert!(images.iter().all(|m| m.path.starts_with("images/")));

        let docs = storage.list_objects(Some("docs/")).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].path, "docs/readme.md");
    }

    #[test]
    fn test_list_empty_storage() {
        let (storage, _tmp) = create_storage();

        let objects = storage.list_objects(None).unwrap();
        assert_eq!(objects.len(), 0);
    }

    #[test]
    fn test_path_traversal_prevention() {
        let (storage, _tmp) = create_storage();

        // Test various path traversal attempts
        let attack_paths = [
            "../etc/passwd",
            "../../etc/passwd",
            "test/../../../etc/passwd",
            "/etc/passwd",
            "test/../../etc/passwd",
        ];

        for path in &attack_paths {
            let result = storage.put_object(path, b"attack", None);
            assert!(result.is_err(), "Path traversal not prevented for: {path}");
        }
    }

    #[test]
    fn test_empty_path_rejection() {
        let (storage, _tmp) = create_storage();

        let result = storage.put_object("", b"data", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_nested_paths() {
        let (storage, _tmp) = create_storage();

        let data = b"nested data";
        storage.put_object("a/b/c/d/file.txt", data, None).unwrap();

        let (retrieved, _) = storage.get_object("a/b/c/d/file.txt").unwrap().unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_content_type_auto_detection() {
        let (storage, _tmp) = create_storage();

        storage.put_object("test.json", b"{}", None).unwrap();
        storage.put_object("test.html", b"<html>", None).unwrap();
        storage.put_object("test.png", b"png", None).unwrap();

        let json_meta = storage.head_object("test.json").unwrap().unwrap();
        assert_eq!(json_meta.content_type, "application/json");

        let html_meta = storage.head_object("test.html").unwrap().unwrap();
        assert_eq!(html_meta.content_type, "text/html");

        let png_meta = storage.head_object("test.png").unwrap().unwrap();
        assert_eq!(png_meta.content_type, "image/png");
    }

    #[test]
    fn test_overwrite_object() {
        let (storage, _tmp) = create_storage();

        storage
            .put_object("test.txt", b"original", Some("text/plain"))
            .unwrap();
        storage
            .put_object("test.txt", b"updated", Some("text/plain"))
            .unwrap();

        let (data, meta) = storage.get_object("test.txt").unwrap().unwrap();
        assert_eq!(data, b"updated");
        assert_eq!(meta.size, 7);
    }

    #[test]
    fn test_normalize_current_dir() {
        let (storage, _tmp) = create_storage();

        storage.put_object("./test.txt", b"data", None).unwrap();

        // Should be stored as "test.txt" without "./"
        let result = storage.get_object("test.txt").unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_list_sorting() {
        let (storage, _tmp) = create_storage();

        storage.put_object("z.txt", b"z", None).unwrap();
        storage.put_object("a.txt", b"a", None).unwrap();
        storage.put_object("m.txt", b"m", None).unwrap();

        let objects = storage.list_objects(None).unwrap();
        assert_eq!(objects[0].path, "a.txt");
        assert_eq!(objects[1].path, "m.txt");
        assert_eq!(objects[2].path, "z.txt");
    }

    #[test]
    fn test_metadata_timestamps() {
        let (storage, _tmp) = create_storage();

        let before = Utc::now();
        let meta = storage.put_object("test.txt", b"data", None).unwrap();
        let after = Utc::now();

        assert!(meta.created_at >= before);
        assert!(meta.created_at <= after);
        assert_eq!(meta.created_at, meta.modified_at);
    }
}
