//! Deterministic disposable Conversation catalog construction.
//!
//! `catalog.json` is never an authority. Rebuild scans canonical dated directories through the
//! shared locator, validates every authoritative JSON/JSONL file, and sorts entries by
//! ConversationId. Its timestamp is derived only from accepted canonical timestamps, so deleting,
//! corrupting, or replacing the cache cannot change the rebuilt bytes.

#[cfg(test)]
use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::conversation::contracts::{
    format_created_at_utc, ConversationErrorCode, ConversationId, ConversationLifecycleState,
    ConversationRecordV2, ConversationTitleSource, CreationPartition, CONVERSATION_SCHEMA_VERSION,
};
use crate::conversation::durable_fs::DurableFileSystem;
use crate::conversation::event_log::{
    scan_event_log, ConversationFrontier, EventLogRepairWarning, EventLogScan,
};
use crate::conversation::locator::{
    bounded_scan, ConversationLocator, LocatorError, MAX_CONVERSATIONS_PER_SCAN,
    MAX_DIRECTORY_ENTRIES_PER_LEVEL,
};

pub const CATALOG_SCHEMA_VERSION: u32 = 1;
pub const CATALOG_FILE: &str = "catalog.json";
pub const CONVERSATION_METADATA_FILE: &str = "conversation.json";
pub const PROVENANCE_FILE: &str = "provenance.json";
pub const EMPTY_CATALOG_GENERATED_AT_UTC: &str = "1970-01-01T00:00:00.000Z";
pub const PROVENANCE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationCatalogEntryV1 {
    pub conversation_id: ConversationId,
    pub created_at_utc: String,
    pub creation_partition: String,
    pub workspace_cwd: String,
    pub project_id: Option<String>,
    pub lifecycle_state: ConversationLifecycleState,
    pub title: Option<String>,
    pub title_source: Option<ConversationTitleSource>,
    pub last_activity_at_utc: String,
    pub message_count: u64,
    pub tool_count: u64,
    pub last_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationCatalogFileV1 {
    pub schema_version: u32,
    pub generated_at_utc: String,
    pub conversations: Vec<ConversationCatalogEntryV1>,
}

impl ConversationCatalogFileV1 {
    #[must_use]
    pub fn deterministic_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .expect("ConversationCatalogFileV1 contains only serializable fields");
        bytes.push(b'\n');
        bytes
    }
}

/// In-memory disposable cache updated from validated Conversation frontiers.
#[derive(Debug, Clone)]
pub struct ConversationCatalog {
    file: ConversationCatalogFileV1,
}

impl ConversationCatalog {
    #[must_use]
    pub fn from_file(file: ConversationCatalogFileV1) -> Self {
        Self { file }
    }

    /// Insert or replace exactly one canonical entry and recompute deterministic cache metadata.
    pub fn upsert(&mut self, record: &ConversationRecordV2, frontier: &ConversationFrontier) {
        let entry = entry_from_frontier(record, frontier);
        self.file.generated_at_utc = self
            .file
            .generated_at_utc
            .clone()
            .max(entry.created_at_utc.clone())
            .max(entry.last_activity_at_utc.clone());
        match self
            .file
            .conversations
            .binary_search_by_key(&record.conversation_id.to_string(), |entry| {
                entry.conversation_id.to_string()
            }) {
            Ok(index) => self.file.conversations[index] = entry,
            Err(index) => self.file.conversations.insert(index, entry),
        }
    }

    #[must_use]
    pub fn deterministic_bytes(&self) -> Vec<u8> {
        self.file.deterministic_bytes()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.file.conversations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.file.conversations.is_empty()
    }
}

#[cfg(test)]
thread_local! {
    static CATALOG_SCAN_COUNT: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_catalog_scan_counter() {
    CATALOG_SCAN_COUNT.set(0);
}

#[cfg(test)]
pub(crate) fn catalog_scan_count() -> u64 {
    CATALOG_SCAN_COUNT.get()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationProvenanceFileV1 {
    pub schema_version: u32,
    pub migration_id: String,
    pub source_records: Vec<ConversationProvenanceSourceV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationProvenanceSourceV1 {
    pub source_kind: String,
    pub relative_path: String,
    pub sha256: String,
    pub preserved_read_only: bool,
}

impl ConversationProvenanceFileV1 {
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.schema_version != PROVENANCE_SCHEMA_VERSION || self.migration_id.trim().is_empty() {
            return Err("invalid provenance schemaVersion or migrationId");
        }
        if self.source_records.iter().any(|source| {
            source.source_kind.trim().is_empty()
                || !valid_relative_source_path(&source.relative_path)
                || !valid_sha256(&source.sha256)
                || !source.preserved_read_only
        }) {
            return Err("provenance contains an invalid source record");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AcceptedCanonicalConversation {
    pub directory: PathBuf,
    pub record: ConversationRecordV2,
    pub scan: EventLogScan,
    pub provenance: Option<ConversationProvenanceFileV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRecoveryIssue {
    pub code: ConversationErrorCode,
    pub conversation_id: Option<ConversationId>,
    pub relative_path: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct CatalogRebuildResult {
    pub catalog: ConversationCatalogFileV1,
    pub accepted: Vec<AcceptedCanonicalConversation>,
    pub recovery_issues: Vec<CatalogRecoveryIssue>,
    pub repairs: Vec<EventLogRepairWarning>,
}

#[derive(Debug)]
pub enum CatalogError {
    Locator(LocatorError),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    ScanBound {
        path: PathBuf,
        limit: usize,
    },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Locator(error) => write!(formatter, "catalog locator failure: {error}"),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "catalog scan I/O failure at '{}': {source}",
                    path.display()
                )
            }
            Self::ScanBound { path, limit } => write!(
                formatter,
                "catalog recovery scan exceeded limit {limit} at '{}'",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<LocatorError> for CatalogError {
    fn from(value: LocatorError) -> Self {
        Self::Locator(value)
    }
}

pub type Result<T> = std::result::Result<T, CatalogError>;

/// Rebuild a catalog exclusively from canonical metadata and validated event streams.
///
/// The returned value is not persisted by this function. Repository open and mutations write the
/// deterministic bytes last, so a cache failure never rolls back authoritative files.
pub fn rebuild_catalog(
    locator: &ConversationLocator,
    durable_fs: &DurableFileSystem,
) -> Result<CatalogRebuildResult> {
    #[cfg(test)]
    CATALOG_SCAN_COUNT.set(CATALOG_SCAN_COUNT.get() + 1);
    let scan = bounded_scan(locator)?;
    let located = scan.collect::<Vec<_>>();
    let accepted_metadata = located
        .iter()
        .map(|entry| entry.relative_directory.clone())
        .collect::<HashSet<_>>();
    let mut recovery_issues = discover_rejected_metadata(locator, &accepted_metadata)?;
    let mut accepted = Vec::new();
    let mut repairs = Vec::new();

    for located in located {
        // Re-derive through the locator before every canonical access rather than trusting a
        // cached or caller-constructed path.
        let directory =
            locator.private_dir(located.conversation_id, &located.creation_partition)?;
        let scan = match scan_event_log(&directory, located.conversation_id, durable_fs) {
            Ok(scan) => scan,
            Err(error) => {
                log::error!(
                    "[conversation-repository] authoritative event log rejected code={} conversation_id={} stream_file={}",
                    stable_code(error.code),
                    located.conversation_id,
                    error.path.file_name().and_then(|name| name.to_str()).unwrap_or("unknown")
                );
                recovery_issues.push(CatalogRecoveryIssue {
                    code: error.code,
                    conversation_id: Some(located.conversation_id),
                    relative_path: relative_display(locator.root(), &error.path),
                    detail: error.detail,
                });
                continue;
            }
        };
        repairs.extend(scan.repairs.clone());
        let provenance = match load_provenance(&directory, located.conversation_id) {
            Ok(provenance) => provenance,
            Err(issue) => {
                recovery_issues.push(issue);
                continue;
            }
        };
        let mut record = located.record;
        record.last_seq = scan.last_seq();
        if scan.frontier.attachment.has_events {
            record.project_attachment = scan.frontier.attachment.current.clone();
        }
        if let Some(lifecycle_state) = scan.frontier.lifecycle_state {
            record.lifecycle_state = lifecycle_state;
        }
        accepted.push(AcceptedCanonicalConversation {
            directory,
            record,
            scan,
            provenance,
        });
    }

    accepted.sort_by_key(|entry| entry.record.conversation_id.to_string());
    recovery_issues.sort_by(|left, right| {
        left.relative_path
            .cmp(&right.relative_path)
            .then_with(|| format!("{:?}", left.code).cmp(&format!("{:?}", right.code)))
    });

    let conversations = accepted
        .iter()
        .map(|entry| entry_from_frontier(&entry.record, &entry.scan.frontier))
        .collect::<Vec<_>>();
    let generated_at_utc = conversations
        .iter()
        .flat_map(|entry| [&entry.created_at_utc, &entry.last_activity_at_utc])
        .max()
        .cloned()
        .unwrap_or_else(|| EMPTY_CATALOG_GENERATED_AT_UTC.to_string());
    Ok(CatalogRebuildResult {
        catalog: ConversationCatalogFileV1 {
            schema_version: CATALOG_SCHEMA_VERSION,
            generated_at_utc,
            conversations,
        },
        accepted,
        recovery_issues,
        repairs,
    })
}

fn entry_from_frontier(
    record: &ConversationRecordV2,
    frontier: &ConversationFrontier,
) -> ConversationCatalogEntryV1 {
    let last_activity_at_utc = frontier
        .summary
        .last_activity_at_utc
        .map_or(record.created_at_utc, |event_time| {
            event_time.max(record.created_at_utc)
        });
    ConversationCatalogEntryV1 {
        conversation_id: record.conversation_id,
        created_at_utc: format_created_at_utc(&record.created_at_utc),
        creation_partition: record.creation_partition.path.clone(),
        workspace_cwd: record.workspace_cwd.clone(),
        project_id: record
            .project_attachment
            .as_ref()
            .map(|attachment| attachment.project_id.clone()),
        lifecycle_state: record.lifecycle_state,
        title: frontier.summary.title.clone(),
        title_source: frontier.summary.title_source,
        last_activity_at_utc: format_created_at_utc(&last_activity_at_utc),
        message_count: frontier.summary.message_count,
        tool_count: frontier.summary.tool_count,
        last_seq: frontier.last_seq,
    }
}

fn load_provenance(
    directory: &Path,
    conversation_id: ConversationId,
) -> std::result::Result<Option<ConversationProvenanceFileV1>, CatalogRecoveryIssue> {
    let path = directory.join(PROVENANCE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(CatalogRecoveryIssue {
                code: ConversationErrorCode::ConversationRecoveryRequired,
                conversation_id: Some(conversation_id),
                relative_path: PROVENANCE_FILE.to_string(),
                detail: format!("provenance cannot be read: {error}"),
            })
        }
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| CatalogRecoveryIssue {
        code: ConversationErrorCode::ConversationRecoveryRequired,
        conversation_id: Some(conversation_id),
        relative_path: PROVENANCE_FILE.to_string(),
        detail: format!("provenance is corrupt: {error}"),
    })?;
    let found = value.get("schemaVersion").and_then(Value::as_u64);
    if found != Some(u64::from(PROVENANCE_SCHEMA_VERSION)) {
        return Err(CatalogRecoveryIssue {
            code: ConversationErrorCode::ConversationUnsupportedSchema,
            conversation_id: Some(conversation_id),
            relative_path: PROVENANCE_FILE.to_string(),
            detail: format!("unsupported provenance schemaVersion {found:?}"),
        });
    }
    let provenance: ConversationProvenanceFileV1 =
        serde_json::from_value(value).map_err(|error| CatalogRecoveryIssue {
            code: ConversationErrorCode::ConversationRecoveryRequired,
            conversation_id: Some(conversation_id),
            relative_path: PROVENANCE_FILE.to_string(),
            detail: format!("invalid provenance record: {error}"),
        })?;
    if let Err(detail) = provenance.validate() {
        return Err(CatalogRecoveryIssue {
            code: ConversationErrorCode::ConversationRecoveryRequired,
            conversation_id: Some(conversation_id),
            relative_path: PROVENANCE_FILE.to_string(),
            detail: detail.to_string(),
        });
    }
    Ok(Some(provenance))
}

fn valid_relative_source_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `bounded_scan` intentionally exposes only accepted records. This fixed-depth companion walk is
/// recovery-report-only: it uses `ConversationLocator::locate_relative` for structural authority,
/// never accepts a record, and exists so unsupported/corrupt authoritative metadata receives a
/// stable item instead of disappearing behind a rejected-count aggregate.
fn discover_rejected_metadata(
    locator: &ConversationLocator,
    accepted: &HashSet<PathBuf>,
) -> Result<Vec<CatalogRecoveryIssue>> {
    if !locator.root().exists() {
        return Ok(Vec::new());
    }
    let mut issues = Vec::new();
    let mut candidates = 0usize;
    for year in read_sorted_dirs(locator.root())? {
        for month in read_sorted_dirs(&year)? {
            for day in read_sorted_dirs(&month)? {
                for directory in read_sorted_dirs(&day)? {
                    candidates += 1;
                    if candidates > MAX_CONVERSATIONS_PER_SCAN {
                        return Err(CatalogError::ScanBound {
                            path: directory,
                            limit: MAX_CONVERSATIONS_PER_SCAN,
                        });
                    }
                    let Ok(relative) = directory.strip_prefix(locator.root()) else {
                        continue;
                    };
                    let Ok((conversation_id, partition)) = locator.locate_relative(relative) else {
                        continue;
                    };
                    if accepted.contains(relative) {
                        continue;
                    }
                    let canonical = locator.private_dir(conversation_id, &partition)?;
                    if canonical != directory {
                        continue;
                    }
                    if let Some(issue) = inspect_rejected_metadata(
                        locator.root(),
                        &directory,
                        relative,
                        conversation_id,
                        &partition,
                    )? {
                        issues.push(issue);
                    }
                }
            }
        }
    }
    Ok(issues)
}

fn read_sorted_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let reader = fs::read_dir(path).map_err(|source| CatalogError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut directories = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|source| CatalogError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if directories.len() >= MAX_DIRECTORY_ENTRIES_PER_LEVEL {
            return Err(CatalogError::ScanBound {
                path: path.to_path_buf(),
                limit: MAX_DIRECTORY_ENTRIES_PER_LEVEL,
            });
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|source| CatalogError::Io {
            path: entry.path(),
            source,
        })?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            directories.push(entry.path());
        }
    }
    directories.sort();
    Ok(directories)
}

fn inspect_rejected_metadata(
    root: &Path,
    directory: &Path,
    relative: &Path,
    conversation_id: ConversationId,
    partition: &CreationPartition,
) -> Result<Option<CatalogRecoveryIssue>> {
    let path = directory.join(CONVERSATION_METADATA_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok(Some(CatalogRecoveryIssue {
                code: ConversationErrorCode::ConversationRecoveryRequired,
                conversation_id: Some(conversation_id),
                relative_path: relative_display(root, &path),
                detail: format!("conversation.json cannot be read: {error}"),
            }))
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return Ok(Some(CatalogRecoveryIssue {
                code: ConversationErrorCode::ConversationRecoveryRequired,
                conversation_id: Some(conversation_id),
                relative_path: relative_display(root, &path),
                detail: format!("conversation.json is corrupt: {error}"),
            }))
        }
    };
    let found = value.get("schemaVersion").and_then(Value::as_u64);
    if found != Some(u64::from(CONVERSATION_SCHEMA_VERSION)) {
        return Ok(Some(CatalogRecoveryIssue {
            code: ConversationErrorCode::ConversationUnsupportedSchema,
            conversation_id: Some(conversation_id),
            relative_path: relative_display(root, &path),
            detail: format!("unsupported conversation schemaVersion {found:?}"),
        }));
    }
    let record: ConversationRecordV2 = match serde_json::from_value(value) {
        Ok(record) => record,
        Err(error) => {
            return Ok(Some(CatalogRecoveryIssue {
                code: ConversationErrorCode::ConversationRecoveryRequired,
                conversation_id: Some(conversation_id),
                relative_path: relative_display(root, &path),
                detail: format!("invalid conversation v2 metadata: {error}"),
            }))
        }
    };
    let valid = record.conversation_id == conversation_id
        && record.creation_partition == *partition
        && CreationPartition::from_created_at(record.created_at_utc) == *partition;
    if valid {
        // A valid metadata record rejected by bounded_scan should still fail closed rather than be
        // silently admitted through this reporting-only path.
        Ok(Some(CatalogRecoveryIssue {
            code: ConversationErrorCode::ConversationRecoveryRequired,
            conversation_id: Some(conversation_id),
            relative_path: relative.to_string_lossy().replace('\\', "/"),
            detail: "canonical directory was rejected by bounded validation".to_string(),
        }))
    } else {
        Ok(Some(CatalogRecoveryIssue {
            code: ConversationErrorCode::ConversationRecoveryRequired,
            conversation_id: Some(conversation_id),
            relative_path: relative_display(root, &path),
            detail: "conversation identity, timestamp, or partition does not match its directory"
                .to_string(),
        }))
    }
}

fn stable_code(code: ConversationErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "CONVERSATION_RECOVERY_REQUIRED".to_string())
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::contracts::{
        parse_created_at_utc, ConversationCreator, ExecutionTarget,
    };
    use crate::conversation::durable_fs::DirectoryPermissions;
    use crate::conversation::event_log::{
        ConversationEventRecordV2, ConversationEventType, EVENT_LOG_FILES,
    };
    use serde_json::json;
    use tempfile::TempDir;

    const FIRST: &str = "018f7a1c-1b4d-7c8a-9f01-0123456789ab";
    const SECOND: &str = "028f7a1c-1b4d-7c8a-9f01-0123456789ab";

    fn fixture() -> (TempDir, ConversationLocator, DurableFileSystem) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("conversations");
        let durable_fs = DurableFileSystem::new();
        durable_fs
            .create_dir_durable(&root, DirectoryPermissions::PrivateOwnerOnly)
            .unwrap();
        let locator = ConversationLocator::new(root).unwrap();
        (temp, locator, durable_fs)
    }

    fn record(id: &str, created_at: &str) -> ConversationRecordV2 {
        let created_at_utc = parse_created_at_utc(created_at).unwrap();
        ConversationRecordV2 {
            schema_version: CONVERSATION_SCHEMA_VERSION,
            conversation_id: ConversationId::parse(id).unwrap(),
            created_at_utc,
            creation_partition: CreationPartition::from_created_at(created_at_utc),
            workspace_cwd: format!("/visible/sessions/{id}"),
            execution_target: ExecutionTarget::Workspace,
            project_attachment: None,
            lifecycle_state: ConversationLifecycleState::Ready,
            last_seq: 0,
            created_by: ConversationCreator::Termul,
        }
    }

    fn write_conversation(
        locator: &ConversationLocator,
        durable_fs: &DurableFileSystem,
        record: &ConversationRecordV2,
    ) -> PathBuf {
        let directory = locator
            .private_dir(record.conversation_id, &record.creation_partition)
            .unwrap();
        durable_fs
            .create_dir_durable(&directory, DirectoryPermissions::PrivateOwnerOnly)
            .unwrap();
        durable_fs
            .replace_bytes(
                &directory.join(CONVERSATION_METADATA_FILE),
                &serde_json::to_vec_pretty(record).unwrap(),
            )
            .unwrap();
        for file in EVENT_LOG_FILES {
            durable_fs
                .replace_bytes(&directory.join(file), b"")
                .unwrap();
        }
        directory
    }

    #[test]
    fn empty_catalog_uses_exact_epoch_without_reading_the_clock() {
        let (_temp, locator, durable_fs) = fixture();
        let rebuilt = rebuild_catalog(&locator, &durable_fs).unwrap();
        assert!(rebuilt.catalog.conversations.is_empty());
        assert_eq!(
            rebuilt.catalog.generated_at_utc,
            EMPTY_CATALOG_GENERATED_AT_UTC
        );
        assert_eq!(rebuilt.catalog.schema_version, CATALOG_SCHEMA_VERSION);
    }

    #[test]
    fn incremental_upsert_matches_explicit_rebuild_for_1000_conversations() {
        let (_temp, locator, durable_fs) = fixture();
        let mut records = Vec::new();
        for index in 0..1_000_u64 {
            let id = format!("00000000-0000-4000-8000-{index:012x}");
            let value = record(&id, "2026-08-15T09:45:15.000Z");
            let directory = locator
                .private_dir(value.conversation_id, &value.creation_partition)
                .unwrap();
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join(CONVERSATION_METADATA_FILE),
                serde_json::to_vec_pretty(&value).unwrap(),
            )
            .unwrap();
            for file in EVENT_LOG_FILES {
                fs::write(directory.join(file), b"").unwrap();
            }
            records.push(value);
        }

        let mut incremental = ConversationCatalog::from_file(ConversationCatalogFileV1 {
            schema_version: CATALOG_SCHEMA_VERSION,
            generated_at_utc: EMPTY_CATALOG_GENERATED_AT_UTC.to_string(),
            conversations: Vec::new(),
        });
        for value in records.iter().rev() {
            incremental.upsert(value, &ConversationFrontier::default());
        }
        let rebuilt = rebuild_catalog(&locator, &durable_fs).unwrap();
        assert_eq!(incremental.len(), 1_000);
        assert_eq!(
            incremental.deterministic_bytes(),
            rebuilt.catalog.deterministic_bytes()
        );
    }

    #[test]
    fn catalog_is_sorted_by_id_and_uses_maximum_validated_canonical_timestamp() {
        let (_temp, locator, durable_fs) = fixture();
        let second = record(SECOND, "2026-08-14T09:45:15.000Z");
        let first = record(FIRST, "2026-08-15T09:45:15.000Z");
        write_conversation(&locator, &durable_fs, &second);
        let first_dir = write_conversation(&locator, &durable_fs, &first);
        let event = ConversationEventRecordV2::new(
            first.conversation_id,
            1,
            parse_created_at_utc("2026-08-16T10:00:00.000Z").unwrap(),
            ConversationEventType::MessageChunk,
            json!({"role":"agent"}),
        );
        durable_fs
            .append_jsonl(
                &first_dir.join(event.type_.stream().file_name()),
                &serde_json::to_vec(&event).unwrap(),
            )
            .unwrap();

        let first_build = rebuild_catalog(&locator, &durable_fs).unwrap();
        let second_build = rebuild_catalog(&locator, &durable_fs).unwrap();
        assert_eq!(
            first_build.catalog.deterministic_bytes(),
            second_build.catalog.deterministic_bytes()
        );
        assert_eq!(
            first_build
                .catalog
                .conversations
                .iter()
                .map(|entry| entry.conversation_id.to_string())
                .collect::<Vec<_>>(),
            vec![FIRST, SECOND]
        );
        assert_eq!(
            first_build.catalog.generated_at_utc,
            "2026-08-16T10:00:00.000Z"
        );
        assert_eq!(first_build.catalog.conversations[0].last_seq, 1);
    }

    #[test]
    fn corrupt_log_is_excluded_without_rewriting_authoritative_bytes() {
        let (_temp, locator, durable_fs) = fixture();
        let value = record(FIRST, "2026-08-15T09:45:15.000Z");
        let directory = write_conversation(&locator, &durable_fs, &value);
        let path = directory.join(crate::conversation::event_log::MESSAGES_FILE);
        let bytes = b"{bad}\n{}\n";
        fs::write(&path, bytes).unwrap();

        let rebuilt = rebuild_catalog(&locator, &durable_fs).unwrap();
        assert!(rebuilt.catalog.conversations.is_empty());
        assert_eq!(rebuilt.recovery_issues.len(), 1);
        assert_eq!(
            rebuilt.recovery_issues[0].code,
            ConversationErrorCode::ConversationRecoveryRequired
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn unsupported_metadata_is_reported_and_never_replaced() {
        let (_temp, locator, durable_fs) = fixture();
        let value = record(FIRST, "2026-08-15T09:45:15.000Z");
        let directory = write_conversation(&locator, &durable_fs, &value);
        let path = directory.join(CONVERSATION_METADATA_FILE);
        let mut future = serde_json::to_value(value).unwrap();
        future["schemaVersion"] = json!(99);
        let bytes = serde_json::to_vec(&future).unwrap();
        fs::write(&path, &bytes).unwrap();

        let rebuilt = rebuild_catalog(&locator, &durable_fs).unwrap();
        assert!(rebuilt.catalog.conversations.is_empty());
        assert!(rebuilt.recovery_issues.iter().any(|issue| {
            issue.code == ConversationErrorCode::ConversationUnsupportedSchema
                && issue.conversation_id == Some(ConversationId::parse(FIRST).unwrap())
        }));
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
