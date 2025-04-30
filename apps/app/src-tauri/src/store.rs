use automerge::{sync, AutoCommit, AutomergeError, Change, ReadDoc};
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Custom error type encapsulating various errors that can occur within the Store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Errors originating from file system operations.
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    /// Errors originating from the SQLite database.
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// Errors originating from the Automerge library.
    #[error("Automerge error: {0}")]
    Automerge(#[from] AutomergeError),

    /// Errors occurring when joining Tokio tasks (e.g., panics in spawned tasks).
    #[error("Task join error: {0}")]
    Join(#[from] tokio::task::JoinError),

    /// Error indicating that a requested document could not be found.
    #[error("Document not found: {0}")]
    DocumentNotFound(String),

    /// Error for invalid Automerge change data received.
    #[error("Invalid Automerge change data: {0}")]
    InvalidChangeData(String),

    /// Generic error type for miscellaneous issues.
    #[error("Other error: {0}")]
    Other(String),
}

/// Manages the storage and retrieval of Automerge documents.
///
/// This struct handles:
/// 1.  Persistence of Automerge documents as binary files on disk.
/// 2.  Metadata management (document IDs, modification times) using an SQLite database.
/// 3.  An in-memory cache of document IDs for quick existence checks.
/// 4.  Thread-safe access to documents and metadata using Tokio's synchronization primitives.
#[derive(Debug)]
pub struct Store {
    /// Asynchronously mutex-guarded SQLite database connection for metadata.
    /// Using `Arc<Mutex<...>>` allows sharing the connection safely across threads.
    db_conn: Arc<Mutex<Connection>>,
    /// Path to the directory where Automerge document files are stored.
    data_dir: PathBuf,
    /// Asynchronously readable/writable lock guarding the in-memory cache of document IDs.
    /// `RwLock` allows concurrent reads, improving performance for existence checks.
    docs_cache: RwLock<HashSet<String>>,
}

impl Store {
    /// Initializes a new `Store`.
    ///
    /// Sets up the data directory, initializes the SQLite database and schema,
    /// and populates the in-memory document ID cache.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` if the data directory cannot be determined or created,
    /// or if the database initialization fails.
    pub fn new() -> Result<Self, StoreError> {
        // Determine the application data directory in a cross-platform manner.
        // Uses environment variables and OS-specific conventions.
        let app_data_dir = directories::ProjectDirs::from("com", "lifemanager", "LifeManager")
            .map(|dirs| dirs.data_dir().to_path_buf())
            .ok_or_else(|| StoreError::Other("Failed to get application data directory".into()))?;

        // Define the specific directory for storing Automerge documents.
        let data_dir = app_data_dir.join("automerge_docs");
        // Create the directory if it doesn't exist, including parent directories.
        fs::create_dir_all(&data_dir)?;

        // Initialize the SQLite database connection.
        let db_path = data_dir.join("metadata.db");
        let conn = Connection::open(&db_path)?;

        // Ensure the necessary 'documents' table exists in the database.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS documents (
                id TEXT PRIMARY KEY NOT NULL,
                last_modified INTEGER NOT NULL
            )",
            [],
        )?;

        // Pre-populate the in-memory cache with existing document IDs from the database.
        let mut docs_cache = HashSet::new();
        {
            let mut stmt = conn.prepare("SELECT id FROM documents")?;
            let ids = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for id_result in ids {
                docs_cache.insert(id_result?);
            }
        }

        Ok(Store {
            db_conn: Arc::new(Mutex::new(conn)),
            data_dir,
            docs_cache: RwLock::new(docs_cache),
        })
    }

    /// Constructs the full file path for a given document ID.
    ///
    /// Document files are stored with a `.automerge` extension.
    #[inline]
    fn get_document_path(&self, doc_id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.automerge", doc_id))
    }

    /// Checks if a document with the given ID exists in the cache.
    ///
    /// Acquires a read lock on the cache for efficient concurrent checking.
    async fn doc_exists(&self, doc_id: &str) -> bool {
        let cache = self.docs_cache.read().await;
        cache.contains(doc_id)
    }

    /// Creates a new, empty Automerge document with the specified ID.
    ///
    /// If the document already exists, this operation succeeds without changes.
    /// Performs file I/O and database updates within a blocking task.
    ///
    /// # Arguments
    ///
    /// * `doc_id` - The unique identifier for the new document.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` if file system operations, database updates,
    /// or task spawning fails.
    pub async fn create_document(&self, doc_id: String) -> Result<(), StoreError> {
        // Avoid redundant creation if the document already exists.
        if self.doc_exists(&doc_id).await {
            return Ok(());
        }

        // Clone necessary data for the blocking task.
        let doc_path = self.get_document_path(&doc_id);
        let db_conn_clone = self.db_conn.clone();

        // Perform blocking operations (file I/O, initial Automerge save, DB write)
        // off the main async thread pool to avoid blocking the runtime.
        tokio::task::spawn_blocking(move || {
            // Create a new empty Automerge document and save its initial state.
            let mut doc = AutoCommit::new();
            let initial_bytes = doc.save(); // `save` generates the compact representation.

            // Write the initial document state to the file system.
            fs::write(&doc_path, &initial_bytes)?;

            // Record the new document in the metadata database.
            // Lock the mutex synchronously as we are in a blocking context.
            let conn = db_conn_clone
                .blocking_lock(); // Use blocking lock inside spawn_blocking
            conn.execute(
                "INSERT INTO documents (id, last_modified) VALUES (?1, ?2)
                 ON CONFLICT(id) DO NOTHING", // Use ON CONFLICT to handle rare races gracefully
                params![doc_id, chrono::Utc::now().timestamp_millis()],
            )?;

            Result::<(), StoreError>::Ok(()) // Explicitly type Ok variant for clarity
        })
        .await??; // First '?' awaits the JoinHandle, second '?' handles the inner Result

        // Update the in-memory cache after successful creation.
        // Acquire a write lock to modify the cache.
        let mut cache = self.docs_cache.write().await;
        cache.insert(doc_id); // Add the new ID to the cache.

        Ok(())
    }

    /// Retrieves the full binary content of a specified Automerge document.
    ///
    /// Reads the document file from disk within a blocking task.
    ///
    /// # Arguments
    ///
    /// * `doc_id` - The identifier of the document to retrieve.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::DocumentNotFound` if the ID is not in the cache.
    /// Returns other `StoreError` variants if file reading or task spawning fails.
    pub async fn get_document(&self, doc_id: &str) -> Result<Vec<u8>, StoreError> {
        // Ensure the document is known before attempting to read.
        if !self.doc_exists(doc_id).await {
            return Err(StoreError::DocumentNotFound(doc_id.to_string()));
        }

        let doc_path = self.get_document_path(doc_id);

        // Perform the blocking file read operation in a separate thread.
        let bytes = tokio::task::spawn_blocking(move || fs::read(&doc_path)).await??;

        Ok(bytes)
    }

    /// Applies changes to an existing Automerge document.
    ///
    /// Loads the document, applies the provided changes, saves the updated state,
    /// and updates the metadata database. All potentially blocking operations
    /// (file I/O, Automerge operations, DB write) are performed within a blocking task.
    ///
    /// # Arguments
    ///
    /// * `doc_id` - The identifier of the document to update.
    /// * `changes` - A byte vector representing the Automerge changes to apply.
    ///               This should be in the format produced by `automerge::AutoCommit::save_changes`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::DocumentNotFound` if the ID is not in the cache.
    /// Returns `StoreError::InvalidChangeData` if the `changes` bytes are malformed.
    /// Returns other `StoreError` variants if file I/O, Automerge operations,
    /// database updates, or task spawning fails.
    pub async fn update_document(&self, doc_id: String, changes: Vec<u8>) -> Result<(), StoreError> {
        // Ensure the document exists before proceeding.
        if !self.doc_exists(&doc_id).await {
            return Err(StoreError::DocumentNotFound(doc_id.clone()));
        }

        // Skip processing if there are no changes to apply.
        if changes.is_empty() {
            // Optionally update last_modified time even if no changes? Depends on requirements.
            // For now, we just return early.
            return Ok(());
        }

        let doc_path = self.get_document_path(&doc_id);
        let db_conn_clone = self.db_conn.clone();

        // Perform the complex update logic (read, load, apply, save, DB update)
        // within a blocking task.
        tokio::task::spawn_blocking(move || {
            // Read the current document state from disk.
            let current_bytes = fs::read(&doc_path)?;

            // Load the document into an Automerge instance.
            let mut doc = AutoCommit::load(&current_bytes)?;

            // Decode and apply the incoming changes.
            // `automerge::Change::from_bytes` decodes a single change.
            // If clients might send multiple changes packed together (e.g., from `save_changes`),
            // the receiving logic might need adjustment depending on the exact format sent.
            // Assuming `changes` represents *one* bundle produced by something like `save_changes`.
            // TODO: Clarify the expected format of `changes`. If it's multiple changes,
            // use `automerge::Change::load_chunk` or similar.
            // For now, assuming it's a single change structure.
            let decoded_changes = Change::load_chunk(&changes)
                .map_err(|e| StoreError::InvalidChangeData(e.to_string()))?;

            // Apply the decoded changes to the document.
            // `apply_changes` handles merging and conflict resolution.
            doc.apply_changes(decoded_changes)?;

            // Save the entire updated document state.
            // `save` produces a compact representation of the full document.
            let updated_bytes = doc.save();

            // Write the updated document back to the file system, overwriting the old one.
            fs::write(&doc_path, &updated_bytes)?;

            // Update the last modified timestamp in the metadata database.
            let conn = db_conn_clone.blocking_lock();
            conn.execute(
                "UPDATE documents SET last_modified = ?1 WHERE id = ?2",
                params![chrono::Utc::now().timestamp_millis(), doc_id],
            )?;

            Result::<(), StoreError>::Ok(())
        })
        .await??;

        Ok(())
    }

    /// Returns a list of all known document IDs.
    ///
    /// Reads directly from the in-memory cache for high performance.
    pub async fn list_documents(&self) -> Result<Vec<String>, StoreError> {
        let cache = self.docs_cache.read().await;
        // Clone the IDs from the cache into a new Vec to return.
        Ok(cache.iter().cloned().collect())
    }

    /// Deletes a document and its associated metadata.
    ///
    /// Removes the document file from disk, deletes the corresponding entry
    /// from the database, and removes the ID from the in-memory cache.
    /// File system and database operations are performed in a blocking task.
    ///
    /// # Arguments
    ///
    /// * `doc_id` - The identifier of the document to delete.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::DocumentNotFound` if the ID is not in the cache.
    /// Returns other `StoreError` variants if file deletion, database updates,
    /// or task spawning fails.
    pub async fn delete_document(&self, doc_id: String) -> Result<(), StoreError> {
        // Ensure the document exists before attempting deletion.
        if !self.doc_exists(&doc_id).await {
            return Err(StoreError::DocumentNotFound(doc_id.clone()));
        }

        let doc_path = self.get_document_path(&doc_id);
        let db_conn_clone = self.db_conn.clone();
        let doc_id_clone = doc_id.clone(); // Clone doc_id for the closure

        // Perform blocking I/O (file deletion, DB delete) in a separate thread.
        tokio::task::spawn_blocking(move || {
            // Attempt to remove the document file. Ignore error if it doesn't exist (idempotency).
            match fs::remove_file(&doc_path) {
                Ok(_) => {} // File deleted successfully
                Err(e) if e.kind() == io::ErrorKind::NotFound => {} // File already gone, okay
                Err(e) => return Err(StoreError::Io(e)),          // Other file system error
            }

            // Remove the document entry from the metadata database.
            let conn = db_conn_clone.blocking_lock();
            conn.execute("DELETE FROM documents WHERE id = ?1", params![doc_id_clone])?;

            Result::<(), StoreError>::Ok(())
        })
        .await??;

        // Remove the document ID from the in-memory cache.
        let mut cache = self.docs_cache.write().await;
        cache.remove(&doc_id); // Use the original doc_id here

        Ok(())
    }

    // --- Potential future additions for more advanced CRDT usage ---

    /*
    /// Generates synchronization messages to send to a peer.
    ///
    /// Loads the document and uses Automerge's sync protocol state
    /// to determine what changes need to be sent.
    /// This would be part of implementing peer-to-peer sync.
    ///
    /// # Arguments
    /// * `doc_id` - The ID of the document to sync.
    /// * `sync_state` - The current Automerge sync state for this peer connection.
    ///
    /// # Returns
    /// A tuple containing the updated sync state and an optional sync message to send.
    pub async fn generate_sync_message(
        &self,
        doc_id: &str,
        sync_state: &mut sync::State, // Assuming sync state is managed per-peer
    ) -> Result<Option<Vec<u8>>, StoreError> {
        if !self.doc_exists(doc_id).await {
            return Err(StoreError::DocumentNotFound(doc_id.to_string()));
        }

        let doc_path = self.get_document_path(doc_id);
        let sync_state_clone = sync_state.clone(); // Clone state for blocking task

        let result = tokio::task::spawn_blocking(move || {
            let bytes = fs::read(&doc_path)?;
            let doc = AutoCommit::load(&bytes)?;
            // Note: AutoCommit doesn't directly expose generate_sync_message.
            // We might need to use `doc.document()` to get the underlying `Automerge` instance.
            let underlying_doc = doc.document();
            let message = underlying_doc.generate_sync_message(&mut sync_state_clone); // Use cloned state
            Result::<_, StoreError>::Ok((sync_state_clone, message.map(|m| m.into_bytes())))
        }).await?;

        let (updated_state, message_bytes) = result?;
        *sync_state = updated_state; // Update the original sync state

        Ok(message_bytes)
    }

    /// Receives synchronization messages from a peer and applies them.
    ///
    /// Loads the document, applies changes received from a peer via the sync protocol,
    /// and saves the updated document state.
    /// This is the counterpart to `generate_sync_message`.
    ///
    /// # Arguments
    /// * `doc_id` - The ID of the document being synced.
    /// * `sync_state` - The current Automerge sync state for this peer connection.
    /// * `message` - The sync message received from the peer.
    ///
    /// # Errors
    /// Returns `StoreError` if the document is not found, the message is invalid,
    /// or if any I/O or Automerge operation fails.
    pub async fn receive_sync_message(
        &self,
        doc_id: String,
        sync_state: &mut sync::State, // Assuming sync state is managed per-peer
        message: Vec<u8>,
    ) -> Result<(), StoreError> {
        if !self.doc_exists(&doc_id).await {
             return Err(StoreError::DocumentNotFound(doc_id.clone()));
         }

        let doc_path = self.get_document_path(&doc_id);
        let db_conn_clone = self.db_conn.clone();
        let sync_state_clone = sync_state.clone(); // Clone state for blocking task

        let result = tokio::task::spawn_blocking(move || {
            let current_bytes = fs::read(&doc_path)?;
            let mut doc = AutoCommit::load(&current_bytes)?;
            let underlying_doc = doc.document_mut(); // Need mutable access

            let sync_message = sync::Message::decode(&message)
                .map_err(|e| StoreError::InvalidChangeData(format!("Invalid sync message: {}", e)))?;

            // `receive_sync_message` applies the changes contained within the message.
            underlying_doc.receive_sync_message(&mut sync_state_clone, sync_message)?;

            // Check if the document was actually changed by the sync message
            if underlying_doc.is_changed() { // Check if `receive_sync_message` resulted in changes
                // `AutoCommit` handles the commit implicitly if changes were made.
                // We just need to save the potentially updated document.
                let updated_bytes = doc.save(); // Save the potentially modified document
                fs::write(&doc_path, &updated_bytes)?;

                // Update timestamp only if changes were applied
                let conn = db_conn_clone.blocking_lock();
                conn.execute(
                    "UPDATE documents SET last_modified = ?1 WHERE id = ?2",
                    params![chrono::Utc::now().timestamp_millis(), doc_id],
                )?;
            }
            Result::<_, StoreError>::Ok(sync_state_clone) // Return updated state
        }).await?;

        *sync_state = result?; // Update the original sync state

        Ok(())
    }
    */
}

// Ensure Store can be safely sent across threads if needed (e.g., in Tauri state).
// These are usually automatically implemented if fields are Send + Sync.
// Explicitly stating them can help catch issues if field types change.
unsafe impl Send for Store {}
unsafe impl Sync for Store {}
