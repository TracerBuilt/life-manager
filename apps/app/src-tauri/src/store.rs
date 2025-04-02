use automerge::AutoCommit;
use rusqlite::Connection;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use tokio::sync::Mutex;
use tokio::sync::RwLock;

/// Custom error type for Store operations
#[derive(Debug)]
pub enum StoreError {
    /// File I/O errors
    Io(std::io::Error),
    /// SQLite database errors
    Database(rusqlite::Error),
    /// Automerge-specific errors
    Automerge(automerge::AutomergeError),
    /// The requested document was not found
    DocumentNotFound,
    /// Generic error with message
    Other(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "IO error: {}", e),
            StoreError::Database(e) => write!(f, "Database error: {}", e),
            StoreError::Automerge(e) => write!(f, "Automerge error: {}", e),
            StoreError::DocumentNotFound => write!(f, "Document not found"),
            StoreError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for StoreError {}

// Implement From traits for convenient error conversions
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Database(e)
    }
}

impl From<automerge::AutomergeError> for StoreError {
    fn from(e: automerge::AutomergeError) -> Self {
        StoreError::Automerge(e)
    }
}

/// Store manages Automerge documents with SQLite-based metadata storage
///
/// The Store serves two primary functions:
/// 1. Manages document data on disk using the Automerge CRDT format
/// 2. Tracks document metadata (IDs, modification times) in SQLite
pub struct Store {
    /// Database connection protected by Mutex for async safety
    db_conn: Mutex<Connection>,
    /// Directory where document files are stored
    data_dir: PathBuf,
    /// In-memory cache of document IDs for faster lookups
    docs_cache: RwLock<HashSet<String>>,
}

impl Store {
    /// Creates a new Store instance
    ///
    /// This initializes:
    /// - The data directory where documents will be stored
    /// - The SQLite database for metadata
    /// - An in-memory cache of document IDs
    pub fn new() -> Result<Self, StoreError> {
        // Create data directory in a cross-platform way
        let app_data_dir = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE")) // Fallback for Windows
            .map(PathBuf::from)
            .or_else(|_| {
                if cfg!(target_os = "windows") {
                    // Default Windows app data directory
                    std::env::var("LOCALAPPDATA").map(PathBuf::from)
                } else if cfg!(target_os = "macos") {
                    // Default macOS app data directory
                    Ok(PathBuf::from("/Users")
                        .join(std::env::var("USER")?)
                        .join("Library/Application Support"))
                } else {
                    // Default Linux app data directory
                    Ok(PathBuf::from("/home")
                        .join(std::env::var("USER")?)
                        .join(".local/share"))
                }
            })
            .map_err(|e| StoreError::Other(format!("Failed to get app data directory: {}", e)))?;

        // Create our app-specific data directory
        let data_dir = app_data_dir.join("life-manager").join("data");
        fs::create_dir_all(&data_dir)?;

        // Initialize SQLite database for document metadata
        let db_path = data_dir.join("store.db");
        let conn = Connection::open(&db_path)?;

        // Create document tracking table if it doesn't exist
        conn.execute(
            "CREATE TABLE IF NOT EXISTS documents (
                id TEXT PRIMARY KEY,
                last_modified INTEGER NOT NULL
            )",
            [],
        )?;

        // Load existing document IDs into memory cache
        let stmt_sql = "SELECT id FROM documents";
        let mut docs_cache = HashSet::new();

        {
            let mut stmt = conn.prepare(stmt_sql)?;
            let ids = stmt.query_map([], |row| row.get::<_, String>(0))?;

            for id in ids {
                docs_cache.insert(id?);
            }
        }

        Ok(Store {
            db_conn: Mutex::new(conn),
            data_dir,
            docs_cache: RwLock::new(docs_cache),
        })
    }

    /// Gets the file path for a document with the given ID
    fn get_document_path(&self, doc_id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.automerge", doc_id))
    }

    /// Creates a document - synchronous version for use with spawn_blocking
    ///
    /// This method avoids using async/await since it's meant to be called
    /// inside a tokio::task::spawn_blocking context.
    pub fn create_document_sync(&self, doc_id: &str) -> Result<(), StoreError> {
        // Create a new empty Automerge document
        let mut doc = AutoCommit::new();
        let bytes = doc.save();

        // Save the document to the filesystem
        fs::write(self.get_document_path(doc_id), &bytes)?;

        // Update the database with metadata about the new document
        {
            // Get a mutable reference to the connection inside the mutex
            let mut guard = self.db_conn.blocking_lock();
            let conn = &mut *guard;

            conn.execute(
                "INSERT OR REPLACE INTO documents (id, last_modified) VALUES (?1, ?2)",
                [doc_id, &chrono::Utc::now().timestamp().to_string()],
            )?;
        }

        // Update the in-memory cache
        {
            // Use the blocking version of RwLock since we're in a sync context
            let mut cache = self.docs_cache.blocking_write();
            cache.insert(doc_id.to_string());
        }

        Ok(())
    }

    /// Creates a new document with the given ID
    ///
    /// If a document with the ID already exists, returns success without changing anything.
    pub async fn create_document(&self, doc_id: &str) -> Result<(), StoreError> {
        // Check if document already exists in our cache
        {
            let cache = self.docs_cache.read().await;
            if cache.contains(doc_id) {
                return Ok(()); // Document already exists
            }
        }

        // Create a new empty Automerge document
        let mut doc = AutoCommit::new();
        let bytes = doc.save();

        // Save the document to the filesystem
        fs::write(self.get_document_path(doc_id), &bytes)?;

        // Update the database with metadata about the new document
        {
            let conn = self.db_conn.lock().await;
            conn.execute(
                "INSERT OR REPLACE INTO documents (id, last_modified) VALUES (?1, ?2)",
                [doc_id, &chrono::Utc::now().timestamp().to_string()],
            )?;
        }

        // Update the in-memory cache
        {
            let mut cache = self.docs_cache.write().await;
            cache.insert(doc_id.to_string());
        }

        Ok(())
    }

    /// Retrieves a document as raw bytes
    ///
    /// Returns the serialized Automerge document which can be loaded by clients
    pub async fn get_document(&self, doc_id: &str) -> Result<Vec<u8>, StoreError> {
        // Check if document exists in our cache
        {
            let cache = self.docs_cache.read().await;
            if !cache.contains(doc_id) {
                return Err(StoreError::DocumentNotFound);
            }
        }

        // Read the document from the filesystem
        let path = self.get_document_path(doc_id);
        let bytes = fs::read(&path)?;

        Ok(bytes)
    }

    /// Updates a document by applying changes
    ///
    /// Takes a document ID and serialized Automerge changes to apply
    pub async fn update_document(&self, doc_id: &str, changes: Vec<u8>) -> Result<(), StoreError> {
        // Check if document exists in our cache
        {
            let cache = self.docs_cache.read().await;
            if !cache.contains(doc_id) {
                return Err(StoreError::DocumentNotFound);
            }
        }

        let path = self.get_document_path(doc_id);

        // Load the existing document
        let bytes = fs::read(&path)?;
        let mut doc = AutoCommit::load(&bytes)?;

        // Apply changes if provided
        if !changes.is_empty() {
            // Parse the changes according to Automerge's format
            // This expects changes in the format produced by Automerge.save_changes() on the client
            let change = match automerge::Change::from_bytes(changes) {
                Ok(change) => change,
                Err(err) => return Err(StoreError::Other(format!("Invalid change data: {}", err))),
            };

            // Apply the change to the document
            doc.apply_changes([change])?;
        }

        // Save the updated document back to the filesystem
        let updated_bytes = doc.save();
        fs::write(&path, &updated_bytes)?;

        // Update the last modified time in the database
        {
            let conn = self.db_conn.lock().await;
            conn.execute(
                "UPDATE documents SET last_modified = ?1 WHERE id = ?2",
                [&chrono::Utc::now().timestamp().to_string(), doc_id],
            )?;
        }

        Ok(())
    }

    /// Lists all document IDs
    ///
    /// Returns a vector of all document IDs known to the store
    pub async fn list_documents(&self) -> Result<Vec<String>, StoreError> {
        let cache = self.docs_cache.read().await;
        Ok(cache.iter().cloned().collect())
    }

    /// Deletes a document
    ///
    /// Removes the document from the filesystem, database, and memory cache
    pub async fn delete_document(&self, doc_id: &str) -> Result<(), StoreError> {
        // Check if document exists in our cache
        {
            let cache = self.docs_cache.read().await;
            if !cache.contains(doc_id) {
                return Err(StoreError::DocumentNotFound);
            }
        }

        // Delete the document file from the filesystem
        let path = self.get_document_path(doc_id);
        if path.exists() {
            fs::remove_file(path)?;
        }

        // Remove the document from the database
        {
            let conn = self.db_conn.lock().await;
            conn.execute("DELETE FROM documents WHERE id = ?1", [doc_id])?;
        }

        // Remove the document from the in-memory cache
        {
            let mut cache = self.docs_cache.write().await;
            cache.remove(doc_id);
        }

        Ok(())
    }
}
