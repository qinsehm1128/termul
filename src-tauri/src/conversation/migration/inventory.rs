//! Exact-layout, read-only inventory for legacy Conversation migration inputs.
//!
//! Only the three historical Termul stores are inspected. Project and worktree trees are never
//! traversed, every admitted regular file is hashed through a bounded streaming reader, and the
//! resulting canonical path ordering is the source snapshot used again during verification.

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{MigrationError, MigrationErrorCode, Result};
use crate::conversation::durable_fs::DurableFileSystem;

pub const LEGACY_INVENTORY_SCHEMA_VERSION: u32 = 1;
pub const INVENTORY_FILE: &str = "inventory-v1.json";
const HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacySourceKind {
    LegacyHostSessions,
    LegacyChatHistory,
    LegacyWorkspaceManifests,
}

impl LegacySourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyHostSessions => "legacy_host_sessions",
            Self::LegacyChatHistory => "legacy_chat_history",
            Self::LegacyWorkspaceManifests => "legacy_workspace_manifests",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyInventoryFileV1 {
    pub relative_path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyInventoryRootV1 {
    pub source_kind: LegacySourceKind,
    pub canonical_path: String,
    pub files: Vec<LegacyInventoryFileV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyInventoryV1 {
    pub schema_version: u32,
    pub operation_id: Uuid,
    pub generated_at_utc: String,
    pub roots: Vec<LegacyInventoryRootV1>,
    pub inventory_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRootSpec {
    pub source_kind: LegacySourceKind,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyRootConfiguration {
    pub host_state_root: PathBuf,
    pub standalone_session_roots: Vec<PathBuf>,
    pub standalone_workspace_manifest_roots: Vec<PathBuf>,
}

impl LegacyRootConfiguration {
    #[must_use]
    pub fn known_roots(&self) -> Vec<LegacyRootSpec> {
        let mut roots = vec![
            LegacyRootSpec {
                source_kind: LegacySourceKind::LegacyHostSessions,
                path: self.host_state_root.join("acp-sessions"),
            },
            LegacyRootSpec {
                source_kind: LegacySourceKind::LegacyChatHistory,
                path: self.host_state_root.join("acp-chat-history"),
            },
            LegacyRootSpec {
                source_kind: LegacySourceKind::LegacyWorkspaceManifests,
                path: self.host_state_root.join("workspace-manifests"),
            },
        ];
        roots.extend(
            self.standalone_session_roots
                .iter()
                .cloned()
                .map(|path| LegacyRootSpec {
                    source_kind: LegacySourceKind::LegacyHostSessions,
                    path,
                }),
        );
        roots.extend(
            self.standalone_workspace_manifest_roots
                .iter()
                .cloned()
                .map(|path| LegacyRootSpec {
                    source_kind: LegacySourceKind::LegacyWorkspaceManifests,
                    path,
                }),
        );
        roots
    }
}

/// Inventory exact known files and persist the pre-migration source snapshot.
pub fn inventory_legacy_roots(
    configuration: &LegacyRootConfiguration,
    operation_id: Uuid,
    generated_at_utc: DateTime<Utc>,
    operation_dir: &Path,
) -> Result<LegacyInventoryV1> {
    let mut roots = Vec::new();
    for spec in configuration.known_roots() {
        if !spec.path.exists() {
            continue;
        }
        let canonical = spec
            .path
            .canonicalize()
            .map_err(|error| inventory_error("canonicalize_inventory_root", &spec.path, error))?;
        if !canonical.is_dir() {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationVerificationFailed,
                "inventory",
                format!(
                    "known legacy root '{}' is not a directory",
                    canonical.display()
                ),
            ));
        }
        let mut files = match spec.source_kind {
            LegacySourceKind::LegacyHostSessions => inventory_host_sessions(&canonical)?,
            LegacySourceKind::LegacyChatHistory => inventory_chat_history(&canonical)?,
            LegacySourceKind::LegacyWorkspaceManifests => {
                inventory_workspace_manifests(&canonical)?
            }
        };
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        roots.push(LegacyInventoryRootV1 {
            source_kind: spec.source_kind,
            canonical_path: canonical.to_string_lossy().into_owned(),
            files,
        });
    }
    roots.sort_by(|left, right| {
        left.source_kind
            .cmp(&right.source_kind)
            .then_with(|| left.canonical_path.cmp(&right.canonical_path))
    });
    roots.dedup_by(|left, right| {
        left.source_kind == right.source_kind && left.canonical_path == right.canonical_path
    });

    let generated_at_utc = generated_at_utc.to_rfc3339_opts(SecondsFormat::Millis, true);
    let inventory_sha256 = inventory_digest(operation_id, &generated_at_utc, &roots)?;
    let inventory = LegacyInventoryV1 {
        schema_version: LEGACY_INVENTORY_SCHEMA_VERSION,
        operation_id,
        generated_at_utc,
        roots,
        inventory_sha256,
    };
    write_inventory(operation_dir, &inventory)?;

    let file_count: usize = inventory.roots.iter().map(|root| root.files.len()).sum();
    let byte_count: u64 = inventory
        .roots
        .iter()
        .flat_map(|root| root.files.iter())
        .map(|file| file.size)
        .sum();
    log::info!(
        "[conversation-migration] legacy inventory complete operation_id={} root_count={} file_count={} byte_count={} inventory_digest={}",
        operation_id,
        inventory.roots.len(),
        file_count,
        byte_count,
        digest_prefix(&inventory.inventory_sha256)
    );
    Ok(inventory)
}

pub fn load_inventory(operation_dir: &Path) -> Result<LegacyInventoryV1> {
    let path = operation_dir.join(INVENTORY_FILE);
    let bytes = fs::read(&path).map_err(|error| inventory_error("read_inventory", &path, error))?;
    let inventory: LegacyInventoryV1 = serde_json::from_slice(&bytes).map_err(|error| {
        MigrationError::new(
            MigrationErrorCode::MigrationJournalCorrupt,
            "read_inventory",
            error.to_string(),
        )
    })?;
    if inventory.schema_version != LEGACY_INVENTORY_SCHEMA_VERSION
        || inventory.inventory_sha256
            != inventory_digest(
                inventory.operation_id,
                &inventory.generated_at_utc,
                &inventory.roots,
            )?
    {
        return Err(MigrationError::new(
            MigrationErrorCode::MigrationJournalCorrupt,
            "read_inventory",
            "inventory schema or canonical digest is invalid",
        ));
    }
    Ok(inventory)
}

pub fn hash_file_streaming(path: &Path) -> Result<(u64, String)> {
    let file =
        File::open(path).map_err(|error| inventory_error("open_legacy_source", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| inventory_error("stat_legacy_source", path, error))?;
    if !metadata.is_file() {
        return Err(MigrationError::new(
            MigrationErrorCode::MigrationVerificationFailed,
            "hash_legacy_source",
            format!("legacy source '{}' is not a regular file", path.display()),
        ));
    }
    let mut reader = BufReader::with_capacity(HASH_BUFFER_BYTES, file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| inventory_error("hash_legacy_source", path, error))?;
        if read == 0 {
            break;
        }
        size = size.checked_add(read as u64).ok_or_else(|| {
            MigrationError::new(
                MigrationErrorCode::MigrationVerificationFailed,
                "hash_legacy_source",
                "legacy source size overflow",
            )
        })?;
        hasher.update(&buffer[..read]);
    }
    Ok((size, lower_hex(&hasher.finalize())))
}

fn inventory_host_sessions(root: &Path) -> Result<Vec<LegacyInventoryFileV1>> {
    let mut paths = Vec::new();
    admit_exact_file(root, root.join("sessions.json"), &mut paths)?;
    for entry in sorted_directory_entries(root)? {
        if !entry
            .file_type()
            .map_err(|error| inventory_error("stat_inventory_entry", &entry.path(), error))?
            .is_dir()
        {
            continue;
        }
        let component = entry.file_name().to_string_lossy().into_owned();
        if Uuid::parse_str(&component).is_err() {
            continue;
        }
        let session_dir = entry.path();
        for name in ["metadata.json", "messages.jsonl", "tool-calls.jsonl"] {
            admit_exact_file(root, session_dir.join(name), &mut paths)?;
        }
    }
    hash_paths(root, paths)
}

fn inventory_chat_history(root: &Path) -> Result<Vec<LegacyInventoryFileV1>> {
    let mut paths = Vec::new();
    for name in ["index.json", "legacy-import.json"] {
        admit_exact_file(root, root.join(name), &mut paths)?;
    }
    let payloads = root.join("payloads");
    if payloads.is_dir() {
        for entry in sorted_directory_entries(&payloads)? {
            let path = entry.path();
            if entry
                .file_type()
                .map_err(|error| inventory_error("stat_inventory_entry", &path, error))?
                .is_file()
                && is_canonical_chat_payload_name(&path)
            {
                paths.push(path);
            }
        }
    }
    hash_paths(root, paths)
}

fn is_canonical_chat_payload_name(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };
    path.extension().and_then(|value| value.to_str()) == Some("json")
        && !stem.is_empty()
        && stem.len().is_multiple_of(2)
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn inventory_workspace_manifests(root: &Path) -> Result<Vec<LegacyInventoryFileV1>> {
    let mut paths = Vec::new();
    for entry in sorted_directory_entries(root)? {
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|error| inventory_error("stat_inventory_entry", &path, error))?
            .is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("json")
        {
            paths.push(path);
        }
    }
    hash_paths(root, paths)
}

fn admit_exact_file(root: &Path, path: PathBuf, paths: &mut Vec<PathBuf>) -> Result<()> {
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => paths.push(path),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationVerificationFailed,
                "inventory",
                format!(
                    "legacy source symlink is not admitted under '{}'",
                    root.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(inventory_error("inventory", &path, error)),
    }
    Ok(())
}

fn hash_paths(root: &Path, mut paths: Vec<PathBuf>) -> Result<Vec<LegacyInventoryFileV1>> {
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(root).map_err(|_| {
                MigrationError::new(
                    MigrationErrorCode::MigrationVerificationFailed,
                    "inventory",
                    "known legacy file escaped its root",
                )
            })?;
            let relative_path = relative_path(relative)?;
            let (size, sha256) = hash_file_streaming(&path)?;
            Ok(LegacyInventoryFileV1 {
                relative_path,
                size,
                sha256,
            })
        })
        .collect()
}

fn sorted_directory_entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| inventory_error("read_inventory_directory", path, error))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| inventory_error("read_inventory_directory", path, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn relative_path(path: &Path) -> Result<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            MigrationError::new(
                MigrationErrorCode::MigrationVerificationFailed,
                "inventory",
                "legacy relative path is not UTF-8",
            )
        })?;
    if components
        .iter()
        .any(|component| component.is_empty() || *component == "..")
    {
        return Err(MigrationError::new(
            MigrationErrorCode::MigrationVerificationFailed,
            "inventory",
            "legacy relative path contains an invalid component",
        ));
    }
    Ok(components.join("/"))
}

fn write_inventory(operation_dir: &Path, inventory: &LegacyInventoryV1) -> Result<()> {
    let durable_fs = DurableFileSystem::new();
    durable_fs
        .create_dir_durable(
            operation_dir,
            crate::conversation::durable_fs::DirectoryPermissions::PrivateOwnerOnly,
        )
        .map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationDurabilityFailed,
                "write_inventory",
                error.to_string(),
            )
        })?;
    let mut bytes = serde_json::to_vec_pretty(inventory).map_err(|error| {
        MigrationError::new(
            MigrationErrorCode::MigrationJournalCorrupt,
            "write_inventory",
            error.to_string(),
        )
    })?;
    bytes.push(b'\n');
    durable_fs
        .replace_bytes(&operation_dir.join(INVENTORY_FILE), &bytes)
        .map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationDurabilityFailed,
                "write_inventory",
                error.to_string(),
            )
        })?;
    Ok(())
}

fn inventory_digest(
    operation_id: Uuid,
    generated_at_utc: &str,
    roots: &[LegacyInventoryRootV1],
) -> Result<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CanonicalInventory<'a> {
        schema_version: u32,
        operation_id: Uuid,
        generated_at_utc: &'a str,
        roots: &'a [LegacyInventoryRootV1],
    }
    let bytes = serde_json::to_vec(&CanonicalInventory {
        schema_version: LEGACY_INVENTORY_SCHEMA_VERSION,
        operation_id,
        generated_at_utc,
        roots,
    })
    .map_err(|error| {
        MigrationError::new(
            MigrationErrorCode::MigrationJournalCorrupt,
            "hash_inventory",
            error.to_string(),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(lower_hex(&hasher.finalize()))
}

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest_prefix(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}

fn inventory_error(operation: &'static str, path: &Path, error: io::Error) -> MigrationError {
    MigrationError::new(
        MigrationErrorCode::MigrationVerificationFailed,
        operation,
        format!(
            "legacy source '{}' could not be read: {error}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inventories_only_known_files_with_streaming_hashes_and_sorted_paths() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let host = base.join("host");
        let sessions = host.join("acp-sessions");
        let chat = host.join("acp-chat-history");
        let manifests = host.join("workspace-manifests");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(chat.join("payloads")).unwrap();
        fs::create_dir_all(&manifests).unwrap();
        let session_dir = sessions.join("018f7a1c-1b4d-7c8a-9f01-0123456789ab");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(session_dir.join("metadata.json"), b"metadata").unwrap();
        fs::write(session_dir.join("messages.jsonl"), b"one\ntwo\n").unwrap();
        fs::write(session_dir.join("tool-calls.jsonl"), b"").unwrap();
        fs::write(sessions.join("ignored.txt"), b"ignore").unwrap();
        fs::create_dir_all(sessions.join("not-a-uuid")).unwrap();
        fs::write(sessions.join("not-a-uuid/metadata.json"), b"ignore").unwrap();
        fs::write(
            chat.join("index.json"),
            json!({"schemaVersion": 1}).to_string(),
        )
        .unwrap();
        fs::write(chat.join("payloads/aa.json"), b"payload").unwrap();
        fs::write(chat.join("payloads/not-hex.json"), b"ignore").unwrap();
        fs::create_dir_all(chat.join("payloads/nested")).unwrap();
        fs::write(chat.join("payloads/nested/ignored.json"), b"ignore").unwrap();
        fs::write(manifests.join("project.json"), b"manifest").unwrap();
        fs::create_dir_all(manifests.join("nested")).unwrap();
        fs::write(manifests.join("nested/ignored.json"), b"ignore").unwrap();

        let operation_id = Uuid::new_v4();
        let inventory = inventory_legacy_roots(
            &LegacyRootConfiguration {
                host_state_root: host,
                ..Default::default()
            },
            operation_id,
            Utc::now(),
            &base.join("operation"),
        )
        .unwrap();
        let paths = inventory
            .roots
            .iter()
            .flat_map(|root| root.files.iter().map(|file| file.relative_path.as_str()))
            .collect::<Vec<_>>();
        assert!(paths.contains(&"018f7a1c-1b4d-7c8a-9f01-0123456789ab/metadata.json"));
        assert!(paths.contains(&"payloads/aa.json"));
        assert!(paths.contains(&"project.json"));
        assert!(!paths.iter().any(|path| path.contains("ignored")));
        assert_eq!(load_inventory(&base.join("operation")).unwrap(), inventory);
    }

    #[test]
    fn streaming_hash_matches_sha256_for_large_source() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.jsonl");
        let bytes = vec![0x5a; HASH_BUFFER_BYTES * 3 + 17];
        fs::write(&path, &bytes).unwrap();
        let (size, digest) = hash_file_streaming(&path).unwrap();
        let mut expected = Sha256::new();
        expected.update(&bytes);
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(digest, lower_hex(&expected.finalize()));
    }
}
