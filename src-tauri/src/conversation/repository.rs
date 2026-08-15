//! Sole live writer for canonical Conversation metadata and event history.
//!
//! The repository owns all v2 JSON/JSONL mutations. Legacy stores are intentionally absent from
//! this module. Every canonical path is re-derived through [`ConversationLocator`], appends are
//! serialized by a per-Conversation async mutex, and `catalog.json` is rewritten last as a
//! disposable deterministic cache.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use parking_lot::Mutex as ParkingMutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;

use crate::conversation::catalog::{
    rebuild_catalog, AcceptedCanonicalConversation, CatalogRecoveryIssue,
    ConversationProvenanceFileV1, CATALOG_FILE, CONVERSATION_METADATA_FILE, PROVENANCE_FILE,
};
use crate::conversation::contracts::{
    AgentSessionBinding, AgentSessionBindingState, ConversationErrorCode, ConversationId,
    ConversationLifecycleState, ConversationRecordV2, ExecutionTarget, ProjectAttachment,
    AGENT_SESSION_BINDING_SCHEMA_VERSION, CONVERSATION_SCHEMA_VERSION,
    PROJECT_ATTACHMENT_SCHEMA_VERSION,
};
use crate::conversation::durable_fs::{DirectoryPermissions, DurableFileSystem};
use crate::conversation::event_log::{
    materialize_records, BindingEventPayloadV1, BindingMaterialization,
    BindingReplacementPayloadV1, ConversationEventRecordV2, ConversationEventType,
    ConversationReplay, EventLogRepairWarning, ProjectAttachmentEventPayloadV1,
    CONVERSATION_EVENT_SCHEMA_VERSION, EVENT_LOG_FILES,
};
use crate::conversation::locator::ConversationLocator;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryRecoveryKind {
    TornTailRepaired,
    CorruptAuthoritativeRecord,
    UnsupportedAuthoritativeSchema,
    IncompleteCreationRecovered,
    CatalogIgnored,
    CatalogRewriteFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryRecoveryItem {
    pub code: ConversationErrorCode,
    pub kind: RepositoryRecoveryKind,
    pub conversation_id: Option<ConversationId>,
    pub relative_path: String,
    pub detail: String,
    pub repaired: bool,
    pub requires_action: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryOpenReport {
    pub valid_conversation_count: usize,
    pub recovery_items: Vec<RepositoryRecoveryItem>,
}

#[derive(Debug, Clone, Default)]
pub struct ConversationMetadataUpdate {
    pub lifecycle_state: Option<ConversationLifecycleState>,
    pub execution_target: Option<ExecutionTarget>,
}

#[derive(Debug, Clone)]
struct ConversationState {
    record: ConversationRecordV2,
    replay: ConversationReplay,
    provenance: Option<ConversationProvenanceFileV1>,
}

#[derive(Debug)]
pub struct RepositoryError {
    pub code: ConversationErrorCode,
    pub operation: &'static str,
    pub conversation_id: Option<ConversationId>,
    pub detail: String,
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?} during {}", self.code, self.operation)?;
        if let Some(conversation_id) = self.conversation_id {
            write!(formatter, " for Conversation {conversation_id}")?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl std::error::Error for RepositoryError {}

pub type Result<T> = std::result::Result<T, RepositoryError>;

pub struct ConversationRepository {
    locator: ConversationLocator,
    durable_fs: DurableFileSystem,
    states: ParkingMutex<HashMap<ConversationId, ConversationState>>,
    recovery_by_id: ParkingMutex<HashMap<ConversationId, RepositoryRecoveryItem>>,
    recovery_items: ParkingMutex<Vec<RepositoryRecoveryItem>>,
    conversation_locks: ParkingMutex<HashMap<ConversationId, Arc<TokioMutex<()>>>>,
    catalog_lock: TokioMutex<()>,
}

impl ConversationRepository {
    /// Open the canonical root, repair only torn final JSONL tails, recover stale creation state,
    /// and rewrite the disposable catalog from validated authoritative files.
    pub fn open(private_root: PathBuf) -> Result<(Arc<Self>, RepositoryOpenReport)> {
        let started_at = Instant::now();
        let durable_fs = DurableFileSystem::new();
        durable_fs
            .create_dir_durable(&private_root, DirectoryPermissions::PrivateOwnerOnly)
            .map_err(|error| {
                repository_error(
                    ConversationErrorCode::ConversationDurabilityFailed,
                    "open",
                    None,
                    error.to_string(),
                )
            })?;
        let locator = ConversationLocator::new(private_root.clone()).map_err(|error| {
            repository_error(
                ConversationErrorCode::ConversationPathEscape,
                "open",
                None,
                error.to_string(),
            )
        })?;
        let catalog_path = private_root.join(CATALOG_FILE);
        let previous_catalog = fs::read(&catalog_path).ok();

        let mut rebuilt = rebuild_catalog(&locator, &durable_fs).map_err(|error| {
            repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "open",
                None,
                error.to_string(),
            )
        })?;
        let mut recovery_items = map_catalog_recovery(&rebuilt.recovery_issues);
        recovery_items.extend(map_repairs(&rebuilt.repairs));

        let mut recovered_incomplete = false;
        for accepted in &mut rebuilt.accepted {
            let mut changed = reconcile_metadata(accepted)?;
            if accepted.record.lifecycle_state == ConversationLifecycleState::InitializingAgent {
                accepted.record.lifecycle_state = ConversationLifecycleState::AgentFailed;
                changed = true;
                recovered_incomplete = true;
                log::warn!(
                    "[conversation-repository] incomplete creation recovered conversation_id={}",
                    accepted.record.conversation_id
                );
                recovery_items.push(RepositoryRecoveryItem {
                    code: ConversationErrorCode::ConversationCreateFailed,
                    kind: RepositoryRecoveryKind::IncompleteCreationRecovered,
                    conversation_id: Some(accepted.record.conversation_id),
                    relative_path: accepted.record.creation_partition.path.clone(),
                    detail: "stale initializing_agent state was closed to agent_failed".to_string(),
                    repaired: true,
                    requires_action: false,
                });
            }
            if changed {
                persist_metadata_at(&durable_fs, &accepted.directory, &accepted.record)?;
            }
        }

        if recovered_incomplete {
            rebuilt = rebuild_catalog(&locator, &durable_fs).map_err(|error| {
                repository_error(
                    ConversationErrorCode::ConversationRecoveryRequired,
                    "open",
                    None,
                    error.to_string(),
                )
            })?;
        }

        let catalog_bytes = rebuilt.catalog.deterministic_bytes();
        if previous_catalog
            .as_ref()
            .is_some_and(|previous| previous != &catalog_bytes)
        {
            log::warn!(
                "[conversation-repository] stale or corrupt catalog ignored root={}",
                private_root.display()
            );
            recovery_items.push(RepositoryRecoveryItem {
                code: ConversationErrorCode::ConversationCorrupt,
                kind: RepositoryRecoveryKind::CatalogIgnored,
                conversation_id: None,
                relative_path: CATALOG_FILE.to_string(),
                detail: "catalog contents were ignored and deterministically rebuilt".to_string(),
                repaired: true,
                requires_action: false,
            });
        }
        if let Err(error) = durable_fs.replace_bytes(&catalog_path, &catalog_bytes) {
            log::warn!(
                "[conversation-repository] cache rewrite failure root={} error={}",
                private_root.display(),
                error
            );
            recovery_items.push(RepositoryRecoveryItem {
                code: ConversationErrorCode::ConversationDurabilityFailed,
                kind: RepositoryRecoveryKind::CatalogRewriteFailed,
                conversation_id: None,
                relative_path: CATALOG_FILE.to_string(),
                detail: "canonical data opened but catalog cache rewrite failed".to_string(),
                repaired: false,
                requires_action: false,
            });
        }

        recovery_items.sort_by(|left, right| {
            left.relative_path.cmp(&right.relative_path).then_with(|| {
                left.conversation_id
                    .map(|id| id.to_string())
                    .cmp(&right.conversation_id.map(|id| id.to_string()))
            })
        });
        let states = rebuilt
            .accepted
            .into_iter()
            .map(|accepted| {
                (
                    accepted.record.conversation_id,
                    ConversationState {
                        record: accepted.record,
                        replay: accepted.replay,
                        provenance: accepted.provenance,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let recovery_by_id = recovery_items
            .iter()
            .filter(|item| item.requires_action)
            .filter_map(|item| item.conversation_id.map(|id| (id, item.clone())))
            .collect::<HashMap<_, _>>();
        let report = RepositoryOpenReport {
            valid_conversation_count: states.len(),
            recovery_items: recovery_items.clone(),
        };
        let repository = Arc::new(Self {
            locator,
            durable_fs,
            states: ParkingMutex::new(states),
            recovery_by_id: ParkingMutex::new(recovery_by_id),
            recovery_items: ParkingMutex::new(recovery_items),
            conversation_locks: ParkingMutex::new(HashMap::new()),
            catalog_lock: TokioMutex::new(()),
        });
        log::info!(
            "[conversation-repository] open complete root={} valid_count={} recovery_item_count={} duration_ms={}",
            private_root.display(),
            report.valid_conversation_count,
            report.recovery_items.len(),
            started_at.elapsed().as_millis()
        );
        Ok((repository, report))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.locator.root()
    }

    #[must_use]
    pub fn recovery_report(&self) -> RepositoryOpenReport {
        RepositoryOpenReport {
            valid_conversation_count: self.states.lock().len(),
            recovery_items: self.recovery_items.lock().clone(),
        }
    }

    pub async fn create_conversation(
        self: &Arc<Self>,
        record: ConversationRecordV2,
    ) -> Result<ConversationRecordV2> {
        validate_new_record(&record)?;
        let lock = self.conversation_lock(record.conversation_id);
        let _guard = lock.lock().await;
        if self.states.lock().contains_key(&record.conversation_id)
            || self
                .recovery_by_id
                .lock()
                .contains_key(&record.conversation_id)
        {
            return Err(repository_error(
                ConversationErrorCode::ConversationCreateFailed,
                "create_conversation",
                Some(record.conversation_id),
                "ConversationId already exists".to_string(),
            ));
        }
        let directory = self
            .locator
            .private_dir(record.conversation_id, &record.creation_partition)
            .map_err(|error| {
                repository_error(
                    ConversationErrorCode::ConversationPathEscape,
                    "create_conversation",
                    Some(record.conversation_id),
                    error.to_string(),
                )
            })?;
        if fs::symlink_metadata(&directory).is_ok() {
            return Err(repository_error(
                ConversationErrorCode::ConversationCreateFailed,
                "create_conversation",
                Some(record.conversation_id),
                "canonical directory already exists".to_string(),
            ));
        }
        self.durable_fs
            .create_dir_durable(&directory, DirectoryPermissions::PrivateOwnerOnly)
            .map_err(|error| {
                durability_error("create_conversation", record.conversation_id, error)
            })?;
        // Metadata is authoritative and durable before any stream exists or event can be appended.
        persist_metadata_at(&self.durable_fs, &directory, &record)?;
        for file in EVENT_LOG_FILES {
            self.durable_fs
                .replace_bytes(&directory.join(file), b"")
                .map_err(|error| {
                    durability_error("create_conversation", record.conversation_id, error)
                })?;
        }
        let state = ConversationState {
            record: record.clone(),
            replay: ConversationReplay {
                records: Vec::new(),
                repairs: Vec::new(),
                binding: BindingMaterialization::default(),
                attachment: Default::default(),
            },
            provenance: None,
        };
        self.states.lock().insert(record.conversation_id, state);
        self.refresh_catalog_best_effort().await;
        log::info!(
            "[conversation-repository] conversation created conversation_id={}",
            record.conversation_id
        );
        Ok(record)
    }

    pub fn get_conversation(
        &self,
        conversation_id: ConversationId,
    ) -> Result<ConversationRecordV2> {
        if let Some(item) = self.recovery_by_id.lock().get(&conversation_id).cloned() {
            return Err(repository_error(
                item.code,
                "get_conversation",
                Some(conversation_id),
                item.detail,
            ));
        }
        self.states
            .lock()
            .get(&conversation_id)
            .map(|state| state.record.clone())
            .ok_or_else(|| {
                repository_error(
                    ConversationErrorCode::ConversationNotFound,
                    "get_conversation",
                    Some(conversation_id),
                    "canonical Conversation was not found".to_string(),
                )
            })
    }

    #[must_use]
    pub fn list_conversations(&self) -> Vec<ConversationRecordV2> {
        let mut records = self
            .states
            .lock()
            .values()
            .map(|state| state.record.clone())
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.conversation_id.to_string());
        records
    }

    pub async fn update_metadata(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        update: ConversationMetadataUpdate,
    ) -> Result<ConversationRecordV2> {
        let lock = self.conversation_lock(conversation_id);
        let _guard = lock.lock().await;
        let mut state = self.state(conversation_id, "update_metadata")?;
        if let Some(lifecycle_state) = update.lifecycle_state {
            state.record.lifecycle_state = lifecycle_state;
        }
        if let Some(execution_target) = update.execution_target {
            state.record.execution_target = execution_target;
        }
        self.persist_state_metadata(&state, "update_metadata")?;
        let record = state.record.clone();
        self.states.lock().insert(conversation_id, state);
        self.refresh_catalog_best_effort().await;
        Ok(record)
    }

    pub async fn append_event(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        recorded_at_utc: DateTime<Utc>,
        type_: ConversationEventType,
        payload: Value,
    ) -> Result<ConversationEventRecordV2> {
        let lock = self.conversation_lock(conversation_id);
        let _guard = lock.lock().await;
        let event = self.append_event_locked(conversation_id, recorded_at_utc, type_, payload)?;
        self.refresh_catalog_best_effort().await;
        Ok(event)
    }

    pub fn read_events(
        &self,
        conversation_id: ConversationId,
        after_seq: u64,
    ) -> Result<Vec<ConversationEventRecordV2>> {
        let state = self.state(conversation_id, "read_events")?;
        if after_seq > state.record.last_seq {
            return Err(repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "read_events",
                Some(conversation_id),
                format!(
                    "cursor {after_seq} is ahead of lastSeq {}",
                    state.record.last_seq
                ),
            ));
        }
        Ok(state
            .replay
            .records
            .into_iter()
            .filter(|event| event.seq > after_seq)
            .collect())
    }

    pub fn current_binding(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<AgentSessionBinding>> {
        Ok(self
            .state(conversation_id, "current_binding")?
            .replay
            .binding
            .current)
    }

    pub fn binding_history(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<AgentSessionBinding>> {
        Ok(self
            .state(conversation_id, "binding_history")?
            .replay
            .binding
            .history)
    }

    pub async fn bind_agent_session(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        mut binding: AgentSessionBinding,
        recorded_at_utc: DateTime<Utc>,
    ) -> Result<ConversationEventRecordV2> {
        binding.state = AgentSessionBindingState::Active;
        validate_binding_input(&binding, conversation_id, "bind_agent_session")?;
        self.append_event(
            conversation_id,
            recorded_at_utc,
            ConversationEventType::BindingBound,
            serde_json::to_value(BindingEventPayloadV1 { binding }).map_err(|error| {
                repository_error(
                    ConversationErrorCode::ConversationBindFailed,
                    "bind_agent_session",
                    Some(conversation_id),
                    error.to_string(),
                )
            })?,
        )
        .await
    }

    pub async fn detach_agent_binding(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        recorded_at_utc: DateTime<Utc>,
    ) -> Result<ConversationEventRecordV2> {
        let mut binding = self.current_binding(conversation_id)?.ok_or_else(|| {
            repository_error(
                ConversationErrorCode::ConversationBindFailed,
                "detach_agent_binding",
                Some(conversation_id),
                "no current binding".to_string(),
            )
        })?;
        binding.state = AgentSessionBindingState::Detached;
        self.append_event(
            conversation_id,
            recorded_at_utc,
            ConversationEventType::BindingDetached,
            serde_json::to_value(BindingEventPayloadV1 { binding }).unwrap(),
        )
        .await
    }

    pub async fn rebind_detached_binding(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        recorded_at_utc: DateTime<Utc>,
    ) -> Result<ConversationEventRecordV2> {
        let mut binding = self.current_binding(conversation_id)?.ok_or_else(|| {
            repository_error(
                ConversationErrorCode::ConversationBindFailed,
                "rebind_detached_binding",
                Some(conversation_id),
                "no current binding".to_string(),
            )
        })?;
        binding.state = AgentSessionBindingState::Active;
        self.append_event(
            conversation_id,
            recorded_at_utc,
            ConversationEventType::BindingRebound,
            serde_json::to_value(BindingEventPayloadV1 { binding }).unwrap(),
        )
        .await
    }

    /// Record suspension only after the provider confirms close/suspend success.
    /// A false confirmation performs no append and leaves materialized state unchanged.
    pub async fn suspend_agent_binding(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        provider_confirmed: bool,
        recorded_at_utc: DateTime<Utc>,
    ) -> Result<Option<ConversationEventRecordV2>> {
        if !provider_confirmed {
            return Ok(None);
        }
        let mut binding = self.current_binding(conversation_id)?.ok_or_else(|| {
            repository_error(
                ConversationErrorCode::ConversationBindFailed,
                "suspend_agent_binding",
                Some(conversation_id),
                "no current binding".to_string(),
            )
        })?;
        binding.state = AgentSessionBindingState::Suspended;
        self.append_event(
            conversation_id,
            recorded_at_utc,
            ConversationEventType::BindingSuspended,
            serde_json::to_value(BindingEventPayloadV1 { binding }).unwrap(),
        )
        .await
        .map(Some)
    }

    pub async fn replace_agent_binding(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        mut binding: AgentSessionBinding,
        recorded_at_utc: DateTime<Utc>,
    ) -> Result<ConversationEventRecordV2> {
        validate_binding_input(&binding, conversation_id, "replace_agent_binding")?;
        let mut previous_binding = self.current_binding(conversation_id)?.ok_or_else(|| {
            repository_error(
                ConversationErrorCode::ConversationBindFailed,
                "replace_agent_binding",
                Some(conversation_id),
                "no binding exists to replace".to_string(),
            )
        })?;
        previous_binding.state = AgentSessionBindingState::Replaced;
        binding.state = AgentSessionBindingState::Active;
        let event = self
            .append_event(
                conversation_id,
                recorded_at_utc,
                ConversationEventType::BindingReplaced,
                serde_json::to_value(BindingReplacementPayloadV1 {
                    previous_binding,
                    binding,
                })
                .unwrap(),
            )
            .await?;
        log::info!(
            "[conversation-repository] binding replaced conversation_id={}",
            conversation_id
        );
        Ok(event)
    }

    pub async fn append_project_attachment(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        attachment: ProjectAttachment,
        recorded_at_utc: DateTime<Utc>,
    ) -> Result<ConversationEventRecordV2> {
        if attachment.schema_version != PROJECT_ATTACHMENT_SCHEMA_VERSION {
            return Err(repository_error(
                ConversationErrorCode::ConversationUnsupportedSchema,
                "append_project_attachment",
                Some(conversation_id),
                "unsupported project attachment schemaVersion".to_string(),
            ));
        }
        if self
            .get_conversation(conversation_id)?
            .project_attachment
            .is_some()
        {
            return Err(repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "append_project_attachment",
                Some(conversation_id),
                "a project attachment is already materialized".to_string(),
            ));
        }
        let event = self
            .append_event(
                conversation_id,
                recorded_at_utc,
                ConversationEventType::ProjectAttached,
                serde_json::to_value(ProjectAttachmentEventPayloadV1 { attachment }).unwrap(),
            )
            .await?;
        log::info!(
            "[conversation-repository] project attachment changed conversation_id={} action=attach",
            conversation_id
        );
        Ok(event)
    }

    pub async fn detach_project_attachment(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        recorded_at_utc: DateTime<Utc>,
    ) -> Result<ConversationEventRecordV2> {
        let attachment = self
            .get_conversation(conversation_id)?
            .project_attachment
            .ok_or_else(|| {
                repository_error(
                    ConversationErrorCode::ConversationRecoveryRequired,
                    "detach_project_attachment",
                    Some(conversation_id),
                    "no project attachment is materialized".to_string(),
                )
            })?;
        let event = self
            .append_event(
                conversation_id,
                recorded_at_utc,
                ConversationEventType::ProjectDetached,
                serde_json::to_value(ProjectAttachmentEventPayloadV1 { attachment }).unwrap(),
            )
            .await?;
        log::info!(
            "[conversation-repository] project attachment changed conversation_id={} action=detach",
            conversation_id
        );
        Ok(event)
    }

    pub async fn write_provenance(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        provenance: ConversationProvenanceFileV1,
    ) -> Result<()> {
        provenance.validate().map_err(|detail| {
            repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "write_provenance",
                Some(conversation_id),
                detail.to_string(),
            )
        })?;
        let lock = self.conversation_lock(conversation_id);
        let _guard = lock.lock().await;
        let mut state = self.state(conversation_id, "write_provenance")?;
        let directory = self.conversation_dir(&state.record, "write_provenance")?;
        let path = directory.join(PROVENANCE_FILE);
        let mut bytes = serde_json::to_vec_pretty(&provenance).map_err(|error| {
            repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "write_provenance",
                Some(conversation_id),
                error.to_string(),
            )
        })?;
        bytes.push(b'\n');
        if let Ok(existing) = fs::read(&path) {
            if existing == bytes {
                return Ok(());
            }
            return Err(repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "write_provenance",
                Some(conversation_id),
                "immutable provenance already exists with different bytes".to_string(),
            ));
        }
        self.durable_fs
            .replace_bytes(&path, &bytes)
            .map_err(|error| durability_error("write_provenance", conversation_id, error))?;
        state.provenance = Some(provenance);
        self.states.lock().insert(conversation_id, state);
        self.refresh_catalog_best_effort().await;
        Ok(())
    }

    pub fn read_provenance(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<ConversationProvenanceFileV1>> {
        Ok(self.state(conversation_id, "read_provenance")?.provenance)
    }

    pub async fn sync_conversation(
        self: &Arc<Self>,
        conversation_id: ConversationId,
    ) -> Result<()> {
        let lock = self.conversation_lock(conversation_id);
        let _guard = lock.lock().await;
        let state = self.state(conversation_id, "sync_conversation")?;
        let directory = self.conversation_dir(&state.record, "sync_conversation")?;
        for file in std::iter::once(CONVERSATION_METADATA_FILE).chain(EVENT_LOG_FILES) {
            self.durable_fs
                .sync_file_and_namespace(&directory.join(file))
                .map_err(|error| durability_error("sync_conversation", conversation_id, error))?;
        }
        if directory.join(PROVENANCE_FILE).exists() {
            self.durable_fs
                .sync_file_and_namespace(&directory.join(PROVENANCE_FILE))
                .map_err(|error| durability_error("sync_conversation", conversation_id, error))?;
        }
        log::info!(
            "[conversation-repository] conversation synced conversation_id={}",
            conversation_id
        );
        Ok(())
    }

    /// Explicit startup policy for callers that want both allocation phases closed after their
    /// visible-workspace reconciliation. It never deletes either root or fabricates a binding.
    pub async fn recover_incomplete_conversations(self: &Arc<Self>) -> Result<usize> {
        let ids = self
            .list_conversations()
            .into_iter()
            .filter(|record| {
                matches!(
                    record.lifecycle_state,
                    ConversationLifecycleState::AllocatingWorkspace
                        | ConversationLifecycleState::InitializingAgent
                )
            })
            .map(|record| record.conversation_id)
            .collect::<Vec<_>>();
        for conversation_id in &ids {
            self.update_metadata(
                *conversation_id,
                ConversationMetadataUpdate {
                    lifecycle_state: Some(ConversationLifecycleState::AgentFailed),
                    execution_target: None,
                },
            )
            .await?;
            log::warn!(
                "[conversation-repository] incomplete creation recovered conversation_id={}",
                conversation_id
            );
        }
        Ok(ids.len())
    }

    /// Stage-2 deletion is a durable tombstone only. Physical removal and blocker policy belong to
    /// the later explicit lifecycle service.
    pub async fn mark_deleted(
        self: &Arc<Self>,
        conversation_id: ConversationId,
    ) -> Result<ConversationRecordV2> {
        let record = self
            .update_metadata(
                conversation_id,
                ConversationMetadataUpdate {
                    lifecycle_state: Some(ConversationLifecycleState::Deleted),
                    execution_target: None,
                },
            )
            .await?;
        log::info!(
            "[conversation-repository] conversation deletion tombstoned conversation_id={}",
            conversation_id
        );
        Ok(record)
    }

    fn append_event_locked(
        &self,
        conversation_id: ConversationId,
        recorded_at_utc: DateTime<Utc>,
        type_: ConversationEventType,
        payload: Value,
    ) -> Result<ConversationEventRecordV2> {
        let mut state = self.state(conversation_id, "append_event")?;
        if state.record.lifecycle_state == ConversationLifecycleState::RecoveryRequired
            || state.record.lifecycle_state == ConversationLifecycleState::Deleted
        {
            return Err(repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "append_event",
                Some(conversation_id),
                "Conversation lifecycle does not admit appends".to_string(),
            ));
        }
        let seq = state.record.last_seq.checked_add(1).ok_or_else(|| {
            repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "append_event",
                Some(conversation_id),
                "global sequence overflow".to_string(),
            )
        })?;
        let event = ConversationEventRecordV2 {
            schema_version: CONVERSATION_EVENT_SCHEMA_VERSION,
            conversation_id,
            seq,
            recorded_at_utc,
            type_,
            payload,
        };
        let directory = self.conversation_dir(&state.record, "append_event")?;
        let mut prospective = state.replay.records.clone();
        prospective.push(event.clone());
        let (binding, attachment) = materialize_records(&prospective, conversation_id, &directory)
            .map_err(|error| {
                repository_error(
                    error.code,
                    "append_event",
                    Some(conversation_id),
                    error.detail,
                )
            })?;
        let bytes = serde_json::to_vec(&event).map_err(|error| {
            repository_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "append_event",
                Some(conversation_id),
                error.to_string(),
            )
        })?;
        self.durable_fs
            .append_jsonl(&directory.join(type_.stream().file_name()), &bytes)
            .map_err(|error| durability_error("append_event", conversation_id, error))?;

        state.replay.records = prospective;
        state.replay.binding = binding;
        state.replay.attachment = attachment;
        state.record.last_seq = seq;
        if state.replay.attachment.has_events {
            state.record.project_attachment = state.replay.attachment.current.clone();
        }
        match type_ {
            ConversationEventType::CreationFailed => {
                state.record.lifecycle_state = ConversationLifecycleState::AgentFailed;
            }
            ConversationEventType::BindingBound
            | ConversationEventType::BindingReplaced
            | ConversationEventType::BindingRebound => {
                state.record.lifecycle_state = ConversationLifecycleState::Ready;
            }
            _ => {}
        }
        // The append is already authoritative. Retain its recovered in-memory frontier even if
        // metadata materialization fails, preventing a duplicate seq in the same process.
        self.states.lock().insert(conversation_id, state.clone());
        self.persist_state_metadata(&state, "append_event")?;
        Ok(event)
    }

    fn persist_state_metadata(
        &self,
        state: &ConversationState,
        operation: &'static str,
    ) -> Result<()> {
        let directory = self.conversation_dir(&state.record, operation)?;
        persist_metadata_at(&self.durable_fs, &directory, &state.record)
    }

    fn conversation_dir(
        &self,
        record: &ConversationRecordV2,
        operation: &'static str,
    ) -> Result<PathBuf> {
        self.locator
            .private_dir(record.conversation_id, &record.creation_partition)
            .map_err(|error| {
                repository_error(
                    ConversationErrorCode::ConversationPathEscape,
                    operation,
                    Some(record.conversation_id),
                    error.to_string(),
                )
            })
    }

    fn state(
        &self,
        conversation_id: ConversationId,
        operation: &'static str,
    ) -> Result<ConversationState> {
        if let Some(item) = self.recovery_by_id.lock().get(&conversation_id).cloned() {
            return Err(repository_error(
                item.code,
                operation,
                Some(conversation_id),
                item.detail,
            ));
        }
        self.states
            .lock()
            .get(&conversation_id)
            .cloned()
            .ok_or_else(|| {
                repository_error(
                    ConversationErrorCode::ConversationNotFound,
                    operation,
                    Some(conversation_id),
                    "canonical Conversation was not found".to_string(),
                )
            })
    }

    fn conversation_lock(&self, conversation_id: ConversationId) -> Arc<TokioMutex<()>> {
        let mut locks = self.conversation_locks.lock();
        Arc::clone(
            locks
                .entry(conversation_id)
                .or_insert_with(|| Arc::new(TokioMutex::new(()))),
        )
    }

    async fn refresh_catalog_best_effort(&self) {
        let _guard = self.catalog_lock.lock().await;
        match rebuild_catalog(&self.locator, &self.durable_fs) {
            Ok(rebuilt) => {
                let path = self.locator.root().join(CATALOG_FILE);
                if let Err(error) = self
                    .durable_fs
                    .replace_bytes(&path, &rebuilt.catalog.deterministic_bytes())
                {
                    log::warn!(
                        "[conversation-repository] cache rewrite failure root={} error={}",
                        self.locator.root().display(),
                        error
                    );
                }
            }
            Err(error) => {
                log::warn!(
                    "[conversation-repository] cache rebuild failure root={} error={}",
                    self.locator.root().display(),
                    error
                );
            }
        }
    }
}

fn validate_new_record(record: &ConversationRecordV2) -> Result<()> {
    if record.schema_version != CONVERSATION_SCHEMA_VERSION {
        return Err(repository_error(
            ConversationErrorCode::ConversationUnsupportedSchema,
            "create_conversation",
            Some(record.conversation_id),
            "unsupported conversation schemaVersion".to_string(),
        ));
    }
    if record.last_seq != 0
        || crate::conversation::contracts::CreationPartition::from_created_at(record.created_at_utc)
            != record.creation_partition
        || record.workspace_cwd.is_empty()
    {
        return Err(repository_error(
            ConversationErrorCode::ConversationCreateFailed,
            "create_conversation",
            Some(record.conversation_id),
            "new metadata must have lastSeq 0, a matching UTC partition, and workspaceCwd"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_binding_input(
    binding: &AgentSessionBinding,
    conversation_id: ConversationId,
    operation: &'static str,
) -> Result<()> {
    if binding.schema_version != AGENT_SESSION_BINDING_SCHEMA_VERSION
        || binding.agent_session_id.is_empty()
        || binding.runtime_agent_id.is_empty()
        || binding.stable_agent_namespace.is_empty()
        || binding.execution_cwd.is_empty()
    {
        return Err(repository_error(
            ConversationErrorCode::ConversationBindFailed,
            operation,
            Some(conversation_id),
            "binding has an invalid schema or empty required opaque field".to_string(),
        ));
    }
    Ok(())
}

fn reconcile_metadata(accepted: &mut AcceptedCanonicalConversation) -> Result<bool> {
    let path = accepted.directory.join(CONVERSATION_METADATA_FILE);
    let bytes = fs::read(&path).map_err(|error| {
        repository_error(
            ConversationErrorCode::ConversationRecoveryRequired,
            "open",
            Some(accepted.record.conversation_id),
            error.to_string(),
        )
    })?;
    let current: ConversationRecordV2 = serde_json::from_slice(&bytes).map_err(|error| {
        repository_error(
            ConversationErrorCode::ConversationRecoveryRequired,
            "open",
            Some(accepted.record.conversation_id),
            error.to_string(),
        )
    })?;
    Ok(current != accepted.record)
}

fn persist_metadata_at(
    durable_fs: &DurableFileSystem,
    directory: &Path,
    record: &ConversationRecordV2,
) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|error| {
        repository_error(
            ConversationErrorCode::ConversationRecoveryRequired,
            "persist_metadata",
            Some(record.conversation_id),
            error.to_string(),
        )
    })?;
    bytes.push(b'\n');
    durable_fs
        .replace_bytes(&directory.join(CONVERSATION_METADATA_FILE), &bytes)
        .map_err(|error| durability_error("persist_metadata", record.conversation_id, error))?;
    Ok(())
}

fn map_catalog_recovery(issues: &[CatalogRecoveryIssue]) -> Vec<RepositoryRecoveryItem> {
    issues
        .iter()
        .map(|issue| RepositoryRecoveryItem {
            code: issue.code,
            kind: if issue.code == ConversationErrorCode::ConversationUnsupportedSchema {
                RepositoryRecoveryKind::UnsupportedAuthoritativeSchema
            } else {
                RepositoryRecoveryKind::CorruptAuthoritativeRecord
            },
            conversation_id: issue.conversation_id,
            relative_path: issue.relative_path.clone(),
            detail: issue.detail.clone(),
            repaired: false,
            requires_action: true,
        })
        .collect()
}

fn map_repairs(repairs: &[EventLogRepairWarning]) -> Vec<RepositoryRecoveryItem> {
    repairs
        .iter()
        .map(|repair| RepositoryRecoveryItem {
            code: ConversationErrorCode::ConversationRecoveryRequired,
            kind: RepositoryRecoveryKind::TornTailRepaired,
            conversation_id: Some(repair.conversation_id),
            relative_path: repair.stream.clone(),
            detail: format!(
                "torn final record truncated after preserving {}",
                repair.backup_file
            ),
            repaired: true,
            requires_action: false,
        })
        .collect()
}

fn repository_error(
    code: ConversationErrorCode,
    operation: &'static str,
    conversation_id: Option<ConversationId>,
    detail: String,
) -> RepositoryError {
    if matches!(
        code,
        ConversationErrorCode::ConversationCorrupt
            | ConversationErrorCode::ConversationRecoveryRequired
            | ConversationErrorCode::ConversationUnsupportedSchema
            | ConversationErrorCode::ConversationDurabilityFailed
            | ConversationErrorCode::ConversationDurabilityUnsupported
    ) {
        log::error!(
            "[conversation-repository] operation failed code={} operation={} conversation_id={}",
            stable_code(code),
            operation,
            conversation_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".to_string())
        );
    }
    RepositoryError {
        code,
        operation,
        conversation_id,
        detail,
    }
}

fn stable_code(code: ConversationErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "CONVERSATION_RECOVERY_REQUIRED".to_string())
}

fn durability_error(
    operation: &'static str,
    conversation_id: ConversationId,
    error: impl fmt::Display,
) -> RepositoryError {
    repository_error(
        ConversationErrorCode::ConversationDurabilityFailed,
        operation,
        Some(conversation_id),
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::catalog::{ConversationCatalogFileV1, CATALOG_SCHEMA_VERSION};
    use crate::conversation::contracts::{
        parse_created_at_utc, ConversationCreator, CreationPartition,
        PROJECT_ATTACHMENT_SCHEMA_VERSION,
    };
    use serde_json::json;
    use tempfile::TempDir;
    use uuid::Uuid;

    const ID: &str = "018f7a1c-1b4d-7c8a-9f01-0123456789ab";

    fn fixture() -> (TempDir, Arc<ConversationRepository>) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("private");
        let (repository, report) = ConversationRepository::open(root).unwrap();
        assert_eq!(report.valid_conversation_count, 0);
        (temp, repository)
    }

    fn record() -> ConversationRecordV2 {
        let created_at_utc = parse_created_at_utc("2026-08-15T09:45:15.123Z").unwrap();
        ConversationRecordV2 {
            schema_version: CONVERSATION_SCHEMA_VERSION,
            conversation_id: ConversationId::parse(ID).unwrap(),
            created_at_utc,
            creation_partition: CreationPartition::from_created_at(created_at_utc),
            workspace_cwd: format!("/visible/sessions/2026/08/15/{ID}"),
            execution_target: ExecutionTarget::Workspace,
            project_attachment: None,
            lifecycle_state: ConversationLifecycleState::InitializingAgent,
            last_seq: 0,
            created_by: ConversationCreator::Termul,
        }
    }

    fn time(second: u32) -> DateTime<Utc> {
        parse_created_at_utc(&format!("2026-08-15T09:45:{second:02}.000Z")).unwrap()
    }

    fn binding(id: &str, opaque: &str) -> AgentSessionBinding {
        AgentSessionBinding {
            schema_version: AGENT_SESSION_BINDING_SCHEMA_VERSION,
            binding_id: Uuid::parse_str(id).unwrap(),
            agent_session_id: opaque.to_string(),
            runtime_agent_id: "runtime-1".to_string(),
            stable_agent_namespace: "config:test".to_string(),
            execution_cwd: format!("/visible/sessions/2026/08/15/{ID}"),
            bound_at_utc: time(16),
            state: AgentSessionBindingState::Active,
        }
    }

    #[tokio::test]
    async fn create_writes_exact_canonical_files_only_under_private_root() {
        let (temp, repository) = fixture();
        let workspace = temp.path().join("visible-workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("user-file.txt"), b"keep").unwrap();
        let mut value = record();
        value.workspace_cwd = workspace.to_string_lossy().into_owned();
        repository.create_conversation(value.clone()).await.unwrap();
        let directory = repository
            .locator
            .private_dir(value.conversation_id, &value.creation_partition)
            .unwrap();
        let mut names = fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            vec![
                "attachments.jsonl",
                "bindings.jsonl",
                "conversation.json",
                "messages.jsonl",
                "tool-calls.jsonl",
            ]
        );
        assert_eq!(fs::read(workspace.join("user-file.txt")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&workspace).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn deleting_or_corrupting_catalog_rebuilds_byte_identically_and_cannot_hide_or_invent() {
        let (_temp, repository) = fixture();
        let mut ready = record();
        ready.lifecycle_state = ConversationLifecycleState::Ready;
        repository.create_conversation(ready).await.unwrap();
        let catalog_path = repository.root().join(CATALOG_FILE);
        let expected = fs::read(&catalog_path).unwrap();
        fs::remove_file(&catalog_path).unwrap();
        let (reopened, _) = ConversationRepository::open(repository.root().to_path_buf()).unwrap();
        assert_eq!(fs::read(&catalog_path).unwrap(), expected);
        assert_eq!(reopened.list_conversations().len(), 1);

        fs::write(
            &catalog_path,
            br#"{"schemaVersion":1,"generatedAtUtc":"2099-01-01T00:00:00.000Z","conversations":[{"conversationId":"ffffffff-ffff-4fff-8fff-ffffffffffff"}]}"#,
        )
        .unwrap();
        let (reopened, report) =
            ConversationRepository::open(repository.root().to_path_buf()).unwrap();
        assert_eq!(fs::read(&catalog_path).unwrap(), expected);
        assert_eq!(
            reopened.list_conversations()[0].conversation_id.to_string(),
            ID
        );
        assert!(report
            .recovery_items
            .iter()
            .any(|item| item.kind == RepositoryRecoveryKind::CatalogIgnored));
        let decoded: ConversationCatalogFileV1 = serde_json::from_slice(&expected).unwrap();
        assert_eq!(decoded.schema_version, CATALOG_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn global_sequence_is_serialized_across_message_and_tool_streams() {
        let (_temp, repository) = fixture();
        repository.create_conversation(record()).await.unwrap();
        let mut tasks = Vec::new();
        for index in 0..20u32 {
            let repository = Arc::clone(&repository);
            tasks.push(tokio::spawn(async move {
                let type_ = if index % 2 == 0 {
                    ConversationEventType::MessageChunk
                } else {
                    ConversationEventType::ToolCall
                };
                repository
                    .append_event(
                        ConversationId::parse(ID).unwrap(),
                        time(20),
                        type_,
                        json!({"structural":"only"}),
                    )
                    .await
                    .unwrap()
                    .seq
            }));
        }
        let mut sequences = Vec::new();
        for task in tasks {
            sequences.push(task.await.unwrap());
        }
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=20).collect::<Vec<_>>());
        assert_eq!(
            repository
                .get_conversation(ConversationId::parse(ID).unwrap())
                .unwrap()
                .last_seq,
            20
        );
    }

    #[tokio::test]
    async fn binding_history_distinguishes_detach_rebound_suspend_and_replacement() {
        let (_temp, repository) = fixture();
        let original_record = record();
        let workspace_cwd = original_record.workspace_cwd.clone();
        repository
            .create_conversation(original_record.clone())
            .await
            .unwrap();
        let conversation_id = original_record.conversation_id;
        let first = binding("b2832b54-2ca4-4db4-93fd-f93bf6793114", "agent/opaque:first");
        repository
            .bind_agent_session(conversation_id, first.clone(), time(16))
            .await
            .unwrap();
        repository
            .detach_agent_binding(conversation_id, time(17))
            .await
            .unwrap();
        assert_eq!(
            repository
                .current_binding(conversation_id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionBindingState::Detached
        );
        repository
            .rebind_detached_binding(conversation_id, time(18))
            .await
            .unwrap();
        assert_eq!(
            repository
                .current_binding(conversation_id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionBindingState::Active
        );
        let before_failed_suspend = repository.read_events(conversation_id, 0).unwrap();
        assert!(repository
            .suspend_agent_binding(conversation_id, false, time(19))
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            repository.read_events(conversation_id, 0).unwrap(),
            before_failed_suspend
        );
        assert_eq!(
            repository
                .current_binding(conversation_id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionBindingState::Active
        );
        repository
            .suspend_agent_binding(conversation_id, true, time(20))
            .await
            .unwrap();
        assert_eq!(
            repository
                .current_binding(conversation_id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionBindingState::Suspended
        );
        let second = binding(
            "c3943c65-3db5-4ec5-a4e0-0a4cf78a4225",
            "agent/opaque:second",
        );
        repository
            .replace_agent_binding(conversation_id, second.clone(), time(21))
            .await
            .unwrap();
        let history = repository.binding_history(conversation_id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].agent_session_id, "agent/opaque:first");
        assert_eq!(history[0].state, AgentSessionBindingState::Replaced);
        assert_eq!(history[1].agent_session_id, "agent/opaque:second");
        assert_eq!(history[1].state, AgentSessionBindingState::Active);
        let replacement = repository
            .read_events(conversation_id, 0)
            .unwrap()
            .into_iter()
            .find(|event| event.type_ == ConversationEventType::BindingReplaced)
            .unwrap();
        assert_eq!(
            replacement.payload["previousBinding"]["agentSessionId"],
            "agent/opaque:first"
        );
        let after = repository.get_conversation(conversation_id).unwrap();
        assert_eq!(after.conversation_id, conversation_id);
        assert_eq!(after.workspace_cwd, workspace_cwd);
    }

    #[tokio::test]
    async fn project_attach_and_detach_change_only_attachment_materialization() {
        let (_temp, repository) = fixture();
        let value = record();
        let conversation_id = value.conversation_id;
        let workspace_cwd = value.workspace_cwd.clone();
        let identity = (
            value.conversation_id,
            value.created_at_utc,
            value.creation_partition.clone(),
        );
        repository.create_conversation(value).await.unwrap();
        let attachment = ProjectAttachment {
            schema_version: PROJECT_ATTACHMENT_SCHEMA_VERSION,
            project_id: "project-opaque".to_string(),
            attached_at_utc: time(22),
            project_path_snapshot: "/projects/example".to_string(),
            worktree_path: None,
            worktree_branch: None,
        };
        repository
            .append_project_attachment(conversation_id, attachment.clone(), time(22))
            .await
            .unwrap();
        let attached = repository.get_conversation(conversation_id).unwrap();
        assert_eq!(attached.project_attachment, Some(attachment));
        assert_eq!(attached.workspace_cwd, workspace_cwd);
        assert_eq!(
            (
                attached.conversation_id,
                attached.created_at_utc,
                attached.creation_partition.clone()
            ),
            identity
        );
        repository
            .detach_project_attachment(conversation_id, time(23))
            .await
            .unwrap();
        let detached = repository.get_conversation(conversation_id).unwrap();
        assert_eq!(detached.project_attachment, None);
        assert_eq!(detached.workspace_cwd, workspace_cwd);
        assert_eq!(
            (
                detached.conversation_id,
                detached.created_at_utc,
                detached.creation_partition
            ),
            identity
        );
    }

    #[tokio::test]
    async fn torn_tail_is_repaired_but_middle_corruption_surfaces_recovery_required() {
        use std::io::Write;

        let (_temp, repository) = fixture();
        repository.create_conversation(record()).await.unwrap();
        repository
            .append_event(
                ConversationId::parse(ID).unwrap(),
                time(16),
                ConversationEventType::MessageChunk,
                json!({"role":"agent"}),
            )
            .await
            .unwrap();
        let directory = repository
            .locator
            .private_dir(
                ConversationId::parse(ID).unwrap(),
                &record().creation_partition,
            )
            .unwrap();
        let messages = directory.join(crate::conversation::event_log::MESSAGES_FILE);
        fs::OpenOptions::new()
            .append(true)
            .open(&messages)
            .unwrap()
            .write_all(b"{torn")
            .unwrap();
        let (_reopened, report) =
            ConversationRepository::open(repository.root().to_path_buf()).unwrap();
        assert!(report
            .recovery_items
            .iter()
            .any(|item| item.kind == RepositoryRecoveryKind::TornTailRepaired));

        fs::write(&messages, b"{bad}\n{}\n").unwrap();
        let bytes = fs::read(&messages).unwrap();
        let (reopened, report) =
            ConversationRepository::open(repository.root().to_path_buf()).unwrap();
        assert!(reopened.list_conversations().is_empty());
        assert!(report.recovery_items.iter().any(|item| {
            item.code == ConversationErrorCode::ConversationRecoveryRequired && item.requires_action
        }));
        assert_eq!(fs::read(messages).unwrap(), bytes);
    }
}
