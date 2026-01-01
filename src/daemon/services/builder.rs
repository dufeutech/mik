//! Unified builder for initializing embedded services.
//!
//! Provides a fluent API for configuring KV, SQL, and Storage services
//! with custom backends or default file-based implementations.

use super::kv::{KvBackend, KvStore, MemoryBackend as KvMemoryBackend, RedbBackend};
use super::sql::{MemorySqlBackend, SqlBackend, SqlService, SqliteBackend};
use super::storage::{FilesystemBackend, MemoryStorageBackend, StorageBackend, StorageService};
use anyhow::{Context, Result};
use std::path::Path;

/// Builder for initializing embedded services.
///
/// Provides a fluent API for configuring which services to enable and
/// what backends to use. Services can be:
/// - Enabled with file-based persistence (default for CLI)
/// - Enabled with in-memory storage (for testing/embedding)
/// - Enabled with custom backends (Redis, PostgreSQL, S3, etc.)
/// - Disabled entirely
///
/// # Example
///
/// ```ignore
/// use mik::daemon::services::ServicesBuilder;
///
/// // Default file-based services
/// let services = ServicesBuilder::new()
///     .data_dir("~/.mik")?
///     .build()?;
///
/// // In-memory services for testing
/// let services = ServicesBuilder::new()
///     .kv_memory()
///     .sql_memory()?
///     .storage_memory()
///     .build()?;
///
/// // Mixed configuration
/// let services = ServicesBuilder::new()
///     .kv_memory()           // Fast in-memory KV
///     .sql_file("./db.sqlite")?  // Persistent SQL
///     .storage_disabled()    // No storage needed
///     .build()?;
/// ```
#[derive(Default)]
pub struct ServicesBuilder {
    kv: Option<Box<dyn KvBackend>>,
    sql: Option<Box<dyn SqlBackend>>,
    storage: Option<Box<dyn StorageBackend>>,
    kv_disabled: bool,
    sql_disabled: bool,
    storage_disabled: bool,
}

impl ServicesBuilder {
    /// Creates a new builder with no services configured.
    ///
    /// By default, all services are disabled until explicitly configured.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures all services using file-based backends in the given directory.
    ///
    /// This is the standard configuration for CLI usage:
    /// - KV: `{data_dir}/kv.redb`
    /// - SQL: `{data_dir}/data.db`
    /// - Storage: `{data_dir}/storage/`
    ///
    /// # Errors
    ///
    /// Returns an error if any of the backends cannot be initialized.
    pub fn data_dir<P: AsRef<Path>>(self, path: P) -> Result<Self> {
        let path = path.as_ref();

        // Ensure directory exists
        std::fs::create_dir_all(path)
            .with_context(|| format!("Failed to create data directory: {}", path.display()))?;

        let kv_path = path.join("kv.redb");
        let sql_path = path.join("data.db");
        let storage_path = path.join("storage");

        let kv_backend = RedbBackend::open(&kv_path)
            .with_context(|| format!("Failed to open KV database: {}", kv_path.display()))?;

        let sql_backend = SqliteBackend::open(&sql_path)
            .with_context(|| format!("Failed to open SQL database: {}", sql_path.display()))?;

        let storage_backend = FilesystemBackend::open(&storage_path)
            .with_context(|| format!("Failed to open storage: {}", storage_path.display()))?;

        Ok(Self {
            kv: Some(Box::new(kv_backend)),
            sql: Some(Box::new(sql_backend)),
            storage: Some(Box::new(storage_backend)),
            kv_disabled: false,
            sql_disabled: false,
            storage_disabled: false,
        })
    }

    // ========================================================================
    // KV Configuration
    // ========================================================================

    /// Configures KV with a custom backend.
    #[must_use]
    pub fn kv<B: KvBackend>(mut self, backend: B) -> Self {
        self.kv = Some(Box::new(backend));
        self.kv_disabled = false;
        self
    }

    /// Configures KV with a file-based redb backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened.
    pub fn kv_file<P: AsRef<Path>>(mut self, path: P) -> Result<Self> {
        let backend = RedbBackend::open(path)?;
        self.kv = Some(Box::new(backend));
        self.kv_disabled = false;
        Ok(self)
    }

    /// Configures KV with an in-memory backend.
    #[must_use]
    pub fn kv_memory(mut self) -> Self {
        self.kv = Some(Box::new(KvMemoryBackend::new()));
        self.kv_disabled = false;
        self
    }

    /// Disables the KV service.
    #[must_use]
    pub fn kv_disabled(mut self) -> Self {
        self.kv = None;
        self.kv_disabled = true;
        self
    }

    // ========================================================================
    // SQL Configuration
    // ========================================================================

    /// Configures SQL with a custom backend.
    #[must_use]
    pub fn sql<B: SqlBackend>(mut self, backend: B) -> Self {
        self.sql = Some(Box::new(backend));
        self.sql_disabled = false;
        self
    }

    /// Configures SQL with a file-based SQLite backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened.
    pub fn sql_file<P: AsRef<Path>>(mut self, path: P) -> Result<Self> {
        let backend = SqliteBackend::open(path)?;
        self.sql = Some(Box::new(backend));
        self.sql_disabled = false;
        Ok(self)
    }

    /// Configures SQL with an in-memory backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the in-memory database cannot be created.
    pub fn sql_memory(mut self) -> Result<Self> {
        let backend = MemorySqlBackend::new()?;
        self.sql = Some(Box::new(backend));
        self.sql_disabled = false;
        Ok(self)
    }

    /// Disables the SQL service.
    #[must_use]
    pub fn sql_disabled(mut self) -> Self {
        self.sql = None;
        self.sql_disabled = true;
        self
    }

    // ========================================================================
    // Storage Configuration
    // ========================================================================

    /// Configures Storage with a custom backend.
    #[must_use]
    pub fn storage<B: StorageBackend>(mut self, backend: B) -> Self {
        self.storage = Some(Box::new(backend));
        self.storage_disabled = false;
        self
    }

    /// Configures Storage with a file-based backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the storage directory cannot be opened.
    pub fn storage_file<P: AsRef<Path>>(mut self, path: P) -> Result<Self> {
        let backend = FilesystemBackend::open(path)?;
        self.storage = Some(Box::new(backend));
        self.storage_disabled = false;
        Ok(self)
    }

    /// Configures Storage with an in-memory backend.
    #[must_use]
    pub fn storage_memory(mut self) -> Self {
        self.storage = Some(Box::new(MemoryStorageBackend::new()));
        self.storage_disabled = false;
        self
    }

    /// Disables the Storage service.
    #[must_use]
    pub fn storage_disabled(mut self) -> Self {
        self.storage = None;
        self.storage_disabled = true;
        self
    }

    // ========================================================================
    // Build
    // ========================================================================

    /// Builds the configured services.
    ///
    /// Services that were not configured and not explicitly disabled will be `None`.
    pub fn build(self) -> Result<Services> {
        let kv = if self.kv_disabled {
            None
        } else {
            self.kv.map(KvStore::from_boxed)
        };

        let sql = if self.sql_disabled {
            None
        } else {
            self.sql.map(SqlService::from_boxed)
        };

        let storage = if self.storage_disabled {
            None
        } else {
            self.storage.map(StorageService::from_boxed)
        };

        Ok(Services { kv, sql, storage })
    }
}

/// Container for initialized services.
///
/// Services may be `None` if they were disabled or not configured.
///
/// # Example
///
/// ```ignore
/// let services = ServicesBuilder::new()
///     .data_dir("~/.mik")?
///     .build()?;
///
/// // Access services
/// if let Some(kv) = &services.kv {
///     kv.set("key", b"value", None).await?;
/// }
///
/// if let Some(sql) = &services.sql {
///     sql.execute("CREATE TABLE ...", &[]).await?;
/// }
///
/// if let Some(storage) = &services.storage {
///     storage.put_object("file.txt", b"data", None).await?;
/// }
/// ```
#[derive(Clone)]
pub struct Services {
    /// Key-value store service (optional)
    pub kv: Option<KvStore>,
    /// SQL database service (optional)
    pub sql: Option<SqlService>,
    /// Object storage service (optional)
    pub storage: Option<StorageService>,
}

impl Services {
    /// Creates a new Services container with all services initialized
    /// using file-based backends in the given data directory.
    ///
    /// This is a convenience method equivalent to:
    /// ```ignore
    /// ServicesBuilder::new().data_dir(path)?.build()
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if any of the backends cannot be initialized.
    pub fn file<P: AsRef<Path>>(path: P) -> Result<Self> {
        ServicesBuilder::new().data_dir(path)?.build()
    }

    /// Creates a new Services container with all services initialized
    /// using in-memory backends.
    ///
    /// Ideal for testing and embedded applications.
    ///
    /// # Errors
    ///
    /// Returns an error if the in-memory SQL database cannot be created.
    pub fn memory() -> Result<Self> {
        ServicesBuilder::new()
            .kv_memory()
            .sql_memory()?
            .storage_memory()
            .build()
    }

    /// Returns a builder for configuring services.
    pub fn builder() -> ServicesBuilder {
        ServicesBuilder::new()
    }

    /// Returns true if the KV service is available.
    pub fn has_kv(&self) -> bool {
        self.kv.is_some()
    }

    /// Returns true if the SQL service is available.
    pub fn has_sql(&self) -> bool {
        self.sql.is_some()
    }

    /// Returns true if the Storage service is available.
    pub fn has_storage(&self) -> bool {
        self.storage.is_some()
    }
}
