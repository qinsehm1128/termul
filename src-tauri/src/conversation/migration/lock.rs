//! Bootstrap-owned host migration lock.
//!
//! Only bootstrap acquires this create-new process lock. The migration service accepts the guard,
//! validates its canonical host-root identity before journal access, and never acquires it itself.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::conversation::durable_fs::{DirectoryPermissions, DurableFileSystem};

use super::{MigrationError, MigrationErrorCode, Result};

pub const MIGRATION_LOCK_FILE: &str = "conversation-layout-v2.lock";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LockOwnerV1 {
    schema_version: u32,
    owner_token: Uuid,
    process_id: u32,
}

#[derive(Debug, Clone)]
pub struct HostMigrationLock {
    canonical_host_root: PathBuf,
    lock_path: PathBuf,
    #[cfg(test)]
    acquire_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl HostMigrationLock {
    pub fn new(host_state_root: &Path) -> Result<Self> {
        let canonical_host_root = host_state_root.canonicalize().map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationLockInvalid,
                "prepare_lock",
                format!("host-state root cannot be canonicalized: {error}"),
            )
        })?;
        let migration_dir = canonical_host_root.join("conversation-migrations");
        DurableFileSystem::new()
            .create_dir_durable(&migration_dir, DirectoryPermissions::PrivateOwnerOnly)
            .map_err(|error| {
                MigrationError::new(
                    MigrationErrorCode::MigrationDurabilityFailed,
                    "prepare_lock",
                    error.to_string(),
                )
            })?;
        Ok(Self {
            canonical_host_root,
            lock_path: migration_dir.join(MIGRATION_LOCK_FILE),
            #[cfg(test)]
            acquire_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    pub fn acquire(&self) -> Result<HostMigrationLockGuard> {
        #[cfg(test)]
        self.acquire_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let owner_token = Uuid::new_v4();
        let owner = LockOwnerV1 {
            schema_version: 1,
            owner_token,
            process_id: std::process::id(),
        };
        let mut bytes = serde_json::to_vec(&owner).map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationLockInvalid,
                "acquire_lock",
                error.to_string(),
            )
        })?;
        bytes.push(b'\n');
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.lock_path).map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::AlreadyExists {
                MigrationErrorCode::MigrationInProgress
            } else {
                MigrationErrorCode::MigrationLockInvalid
            };
            MigrationError::new(code, "acquire_lock", error.to_string())
        })?;
        if let Err(error) = file
            .write_all(&bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(&self.lock_path);
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationDurabilityFailed,
                "acquire_lock",
                error.to_string(),
            ));
        }
        log::info!(
            "[conversation-migration] host lock acquired root={} operation=bootstrap_handoff",
            self.canonical_host_root.display()
        );
        Ok(HostMigrationLockGuard {
            canonical_host_root: self.canonical_host_root.clone(),
            lock_path: self.lock_path.clone(),
            owner_token,
            file: Some(file),
        })
    }

    #[cfg(test)]
    pub(super) fn acquire_count(&self) -> usize {
        self.acquire_count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub struct HostMigrationLockGuard {
    canonical_host_root: PathBuf,
    lock_path: PathBuf,
    owner_token: Uuid,
    file: Option<File>,
}

impl HostMigrationLockGuard {
    #[must_use]
    pub fn canonical_host_root(&self) -> &Path {
        &self.canonical_host_root
    }

    pub fn validate_host_root(&self, host_state_root: &Path) -> Result<()> {
        let canonical = host_state_root.canonicalize().map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationLockInvalid,
                "validate_lock_guard",
                format!("host-state root cannot be canonicalized: {error}"),
            )
        })?;
        if canonical != self.canonical_host_root {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationLockInvalid,
                "validate_lock_guard",
                "pre-acquired migration guard belongs to a different canonical host-state root",
            ));
        }
        Ok(())
    }
}

impl Drop for HostMigrationLockGuard {
    fn drop(&mut self) {
        // Close the held handle before removing the owner file; Windows rejects unlinking an open
        // file even when the current process owns it.
        drop(self.file.take());
        let owned = fs::read(&self.lock_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<LockOwnerV1>(&bytes).ok())
            .is_some_and(|owner| owner.owner_token == self.owner_token);
        if owned {
            if let Err(error) = fs::remove_file(&self.lock_path) {
                log::error!(
                    "[conversation-migration] host lock release failed root={} error={}",
                    self.canonical_host_root.display(),
                    error
                );
            }
        } else {
            log::error!(
                "[conversation-migration] host lock ownership changed before release root={}",
                self.canonical_host_root.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_new_lock_is_exclusive_and_guard_is_root_bound() {
        let root = tempfile::tempdir().unwrap();
        let canonical = root.path().canonicalize().unwrap();
        let lock = HostMigrationLock::new(&canonical).unwrap();
        let guard = lock.acquire().unwrap();
        assert!(guard.validate_host_root(&canonical).is_ok());
        assert_eq!(
            lock.acquire().unwrap_err().code,
            MigrationErrorCode::MigrationInProgress
        );
        drop(guard);
        assert!(lock.acquire().is_ok());
    }
}
