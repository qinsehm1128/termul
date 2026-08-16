use keyring::Entry;
use serde::{Deserialize, Serialize};

const SERVICE_NAME: &str = "com.termul.manager";

#[derive(Debug, Serialize, Deserialize)]
pub struct SecureStorageRequest {
    pub key: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SecureStorageSetRequest {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SecureStorageResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl<T> SecureStorageResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            code: None,
        }
    }

    pub fn error(error: String, code: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            code: Some(code),
        }
    }
}

impl SecureStorageResponse<()> {
    pub fn success_void() -> Self {
        Self {
            success: true,
            data: None,
            error: None,
            code: None,
        }
    }
}

fn get_entry(key: &str) -> Result<Entry, String> {
    Entry::new(SERVICE_NAME, key).map_err(|e| format!("Failed to create keyring entry: {}", e))
}

/// Crate-private keyring helpers for host-owned credentials. Callers must not
/// log returned values or include them in error messages.
pub(crate) fn keyring_get(key: &str) -> Result<Option<String>, String> {
    match get_entry(key)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("Failed to retrieve keyring credential: {error}")),
    }
}

pub(crate) fn keyring_set(key: &str, value: &str) -> Result<(), String> {
    get_entry(key)?
        .set_password(value)
        .map_err(|error| format!("Failed to store keyring credential: {error}"))
}

pub(crate) fn keyring_delete(key: &str) -> Result<(), String> {
    match get_entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("Failed to delete keyring credential: {error}")),
    }
}

#[tauri::command]
pub fn secure_storage_set(request: SecureStorageSetRequest) -> SecureStorageResponse<()> {
    match keyring_set(&request.key, &request.value) {
        Ok(()) => SecureStorageResponse::success_void(),
        Err(error) => SecureStorageResponse::error(error, "STORAGE_ERROR".to_string()),
    }
}

#[tauri::command]
pub fn secure_storage_get(request: SecureStorageRequest) -> SecureStorageResponse<String> {
    match keyring_get(&request.key) {
        Ok(Some(value)) => SecureStorageResponse::success(value),
        Ok(None) => SecureStorageResponse::error(
            format!("Secret not found for key: {}", request.key),
            "KEY_NOT_FOUND".to_string(),
        ),
        Err(error) => SecureStorageResponse::error(error, "RETRIEVAL_ERROR".to_string()),
    }
}

#[tauri::command]
pub fn secure_storage_delete(request: SecureStorageRequest) -> SecureStorageResponse<()> {
    match keyring_delete(&request.key) {
        Ok(()) => SecureStorageResponse::success_void(),
        Err(error) => SecureStorageResponse::error(error, "DELETE_ERROR".to_string()),
    }
}
