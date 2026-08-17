use keyring::Entry;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

const SERVICE_NAME: &str = "com.termul.manager";
pub(crate) const KEYRING_DELETE_FAILED: &str = "KEYRING_DELETE_FAILED";

#[cfg(test)]
static INJECTED_DELETE_FAILURES: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyringDeleteError;

impl KeyringDeleteError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        KEYRING_DELETE_FAILED
    }
}

impl std::fmt::Display for KeyringDeleteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("keyring credential deletion failed")
    }
}

impl std::error::Error for KeyringDeleteError {}

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
    Entry::new(SERVICE_NAME, key).map_err(|_| "keyring entry is unavailable".to_string())
}

/// Crate-private keyring helpers for host-owned credentials. Callers must not
/// log returned values, account names, or underlying backend errors.
pub(crate) fn keyring_get(key: &str) -> Result<Option<String>, String> {
    match get_entry(key)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("keyring credential retrieval failed".to_string()),
    }
}

pub(crate) fn keyring_set(key: &str, value: &str) -> Result<(), String> {
    get_entry(key)?
        .set_password(value)
        .map_err(|_| "keyring credential storage failed".to_string())
}

/// Delete a host-owned generation credential. A missing entry is idempotent
/// success; every backend/descriptor failure is collapsed to one stable,
/// account-free error for aggregate generation-retirement receipts.
pub(crate) fn keyring_delete_checked(key: &str) -> Result<(), KeyringDeleteError> {
    #[cfg(test)]
    if INJECTED_DELETE_FAILURES
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return Err(KeyringDeleteError);
    }

    let entry = Entry::new(SERVICE_NAME, key).map_err(|_| KeyringDeleteError)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(KeyringDeleteError),
    }
}

pub(crate) fn keyring_delete(key: &str) -> Result<(), String> {
    keyring_delete_checked(key).map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn fail_next_keyring_deletes_for_tests(count: usize) {
    INJECTED_DELETE_FAILURES.store(count, Ordering::SeqCst);
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
