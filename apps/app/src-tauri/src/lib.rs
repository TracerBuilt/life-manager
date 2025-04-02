mod store;

use std::sync::Arc;
use store::{Store, StoreError};
use tauri::State;

// The store will be managed by Tauri
struct AppState {
    store: Arc<Store>,
}

// Commands for interacting with the store
#[tauri::command]
async fn create_document(state: State<'_, AppState>, doc_id: String) -> Result<(), String> {
    // Spawn a blocking task to handle mutex operations, which makes it Send-safe
    let store_clone = state.store.clone();
    let doc_id_clone = doc_id.clone();

    tokio::task::spawn_blocking(move || store_clone.create_document_sync(&doc_id_clone))
        .await
        .unwrap_or_else(|e| Err(StoreError::Other(format!("Task join error: {}", e))))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_document(state: State<'_, AppState>, doc_id: String) -> Result<Vec<u8>, String> {
    state
        .store
        .get_document(&doc_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_document(
    state: State<'_, AppState>,
    doc_id: String,
    changes: Vec<u8>,
) -> Result<(), String> {
    state
        .store
        .update_document(&doc_id, changes)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_documents(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    state
        .store
        .list_documents()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_document(state: State<'_, AppState>, doc_id: String) -> Result<(), String> {
    state
        .store
        .delete_document(&doc_id)
        .await
        .map_err(|e| e.to_string())
}

// Bonus utility command
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize the store
    let store = match Store::new() {
        Ok(store) => Arc::new(store),
        Err(e) => {
            eprintln!("Failed to initialize store: {}", e);
            std::process::exit(1);
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState { store })
        .invoke_handler(tauri::generate_handler![
            greet,
            create_document,
            get_document,
            update_document,
            list_documents,
            delete_document
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
