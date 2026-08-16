//! ACP-to-Conversation persistence adapter.
//!
//! The adapter is the only live ACP history writer after bootstrap. Opaque agent session ids are
//! resolved through canonical binding history; unmapped events fail closed and never fall back to
//! a legacy store.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use serde_json::Value;

use crate::acp::session_persistence::{
    PersistedEventRecord, PersistedSessionStatus, SessionIndexEntry, SessionMetadata, TitleSource,
    SESSION_SCHEMA_VERSION,
};
use crate::conversation::contracts::{
    AgentSessionBindingState, ConversationId, ConversationLifecycleState, ConversationTitleSource,
};
use crate::conversation::event_log::ConversationEventType;
use crate::conversation::migration::ConversationReader;
use crate::conversation::repository::ConversationRepository;

#[derive(Debug)]
pub struct ConversationPersistenceError {
    pub code: &'static str,
    pub operation: &'static str,
    pub detail: String,
}

impl fmt::Display for ConversationPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} during {}: {}",
            self.code, self.operation, self.detail
        )
    }
}

impl std::error::Error for ConversationPersistenceError {}

pub type Result<T> = std::result::Result<T, ConversationPersistenceError>;

pub struct ConversationPersistenceAdapter {
    repository: Arc<ConversationRepository>,
    reader: Arc<ConversationReader>,
    bindings: RwLock<HashMap<String, ConversationId>>,
}

impl ConversationPersistenceAdapter {
    #[must_use]
    pub fn new(repository: Arc<ConversationRepository>, reader: Arc<ConversationReader>) -> Self {
        let adapter = Self {
            repository,
            reader,
            bindings: RwLock::new(HashMap::new()),
        };
        adapter.rebuild_binding_index();
        adapter
    }

    pub fn rebuild_binding_index(&self) {
        let mut bindings = HashMap::new();
        for record in self.repository.list_conversations() {
            if let Ok(Some(binding)) = self.repository.current_binding(record.conversation_id) {
                if binding.state == AgentSessionBindingState::Active {
                    bindings.insert(binding.agent_session_id, record.conversation_id);
                }
            }
        }
        *self.bindings.write() = bindings;
    }

    pub fn register_binding(&self, agent_session_id: &str, conversation_id: ConversationId) {
        self.bindings
            .write()
            .insert(agent_session_id.to_string(), conversation_id);
        log::info!("[conversation-persistence] binding indexed conversation_id={conversation_id}");
    }

    #[must_use]
    pub fn conversation_id_for_session(&self, agent_session_id: &str) -> Option<ConversationId> {
        self.conversation_id_for_active_binding(agent_session_id)
    }

    #[must_use]
    pub fn conversation_id_for_active_binding(
        &self,
        agent_session_id: &str,
    ) -> Option<ConversationId> {
        if let Some(conversation_id) = self.bindings.read().get(agent_session_id).copied() {
            let active = self
                .repository
                .current_binding(conversation_id)
                .ok()
                .flatten()
                .is_some_and(|binding| {
                    binding.state == AgentSessionBindingState::Active
                        && binding.agent_session_id == agent_session_id
                });
            if active {
                return Some(conversation_id);
            }
            self.bindings.write().remove(agent_session_id);
        }
        self.rebuild_binding_index();
        self.bindings.read().get(agent_session_id).copied()
    }

    #[must_use]
    pub fn conversation_id_for_current_binding(
        &self,
        agent_session_id: &str,
    ) -> Option<ConversationId> {
        self.conversation_id_for_history_binding(agent_session_id)
    }

    #[must_use]
    pub fn conversation_id_for_history_binding(
        &self,
        agent_session_id: &str,
    ) -> Option<ConversationId> {
        self.repository
            .list_conversations()
            .into_iter()
            .find_map(|record| {
                self.repository
                    .current_binding(record.conversation_id)
                    .ok()
                    .flatten()
                    .filter(|binding| binding.agent_session_id == agent_session_id)
                    .map(|_| record.conversation_id)
            })
    }

    pub async fn append_acp_event(
        self: &Arc<Self>,
        agent_session_id: &str,
        type_: &str,
        payload: Value,
    ) -> Result<u64> {
        let conversation_id = self
            .conversation_id_for_active_binding(agent_session_id)
            .ok_or_else(|| {
                log::error!(
                    "[conversation-persistence] inactive or unmapped ACP event rejected event_type={type_}"
                );
                error(
                    "CONVERSATION_BINDING_NOT_FOUND",
                    "append_acp_event",
                    "opaque agent session id has no canonical Conversation binding",
                )
            })?;
        let canonical_type = canonical_event_type(type_).ok_or_else(|| {
            error(
                "CONVERSATION_EVENT_UNSUPPORTED",
                "append_acp_event",
                format!("unsupported ACP event type {type_}"),
            )
        })?;
        let event = self
            .repository
            .append_event(conversation_id, Utc::now(), canonical_type, payload)
            .await
            .map_err(|source| {
                error(
                    "CONVERSATION_EVENT_APPEND_FAILED",
                    "append_acp_event",
                    source.to_string(),
                )
            })?;
        Ok(event.seq)
    }

    #[must_use]
    pub fn list_sessions(&self) -> Vec<SessionIndexEntry> {
        let mut sessions = Vec::new();
        for record in self.reader.list() {
            if record.lifecycle_state == ConversationLifecycleState::Deleted {
                continue;
            }
            let Ok(Some(binding)) = self.repository.current_binding(record.conversation_id) else {
                continue;
            };
            let Ok(summary) = self.repository.history_summary(record.conversation_id) else {
                continue;
            };
            let status = history_status(record.lifecycle_state, binding.state);
            sessions.push(SessionIndexEntry {
                storage_key: record.conversation_id.to_string(),
                session_id: binding.agent_session_id,
                stable_agent_namespace: Some(binding.stable_agent_namespace),
                runtime_agent_id: Some(binding.runtime_agent_id),
                project_id: record
                    .project_attachment
                    .as_ref()
                    .map(|attachment| attachment.project_id.clone()),
                cwd: binding.execution_cwd,
                title: summary.title,
                title_source: summary.title_source.map(acp_title_source),
                created_at: record.created_at_utc.timestamp_millis().max(0) as u64,
                last_activity_at: summary.last_activity_at_utc.timestamp_millis().max(0) as u64,
                status,
                message_count: summary.message_count,
                tool_count: summary.tool_count,
                last_seq: record.last_seq,
                discovered: false,
                resume_eligible: true,
                worktree_path: record
                    .project_attachment
                    .as_ref()
                    .and_then(|attachment| attachment.worktree_path.clone()),
                worktree_branch: record
                    .project_attachment
                    .as_ref()
                    .and_then(|attachment| attachment.worktree_branch.clone()),
            });
        }
        sessions
    }

    pub fn legacy_materialization(
        &self,
        agent_session_id: &str,
    ) -> Result<(SessionMetadata, Vec<PersistedEventRecord>)> {
        let conversation_id = self
            .conversation_id_for_history_binding(agent_session_id)
            .ok_or_else(|| error("CONVERSATION_NOT_FOUND", "materialize", "binding not found"))?;
        let record = self.reader.get(conversation_id).map_err(|source| {
            error(
                "CONVERSATION_READ_FAILED",
                "materialize",
                source.to_string(),
            )
        })?;
        let binding = self
            .repository
            .current_binding(conversation_id)
            .map_err(|source| {
                error(
                    "CONVERSATION_READ_FAILED",
                    "materialize_binding",
                    source.to_string(),
                )
            })?
            .ok_or_else(|| {
                error(
                    "CONVERSATION_BINDING_NOT_FOUND",
                    "materialize",
                    "binding not found",
                )
            })?;
        let summary = self
            .repository
            .history_summary(conversation_id)
            .map_err(|source| {
                error(
                    "CONVERSATION_READ_FAILED",
                    "materialize_summary",
                    source.to_string(),
                )
            })?;
        let events = self
            .repository
            .read_events(conversation_id, 0)
            .map_err(|source| {
                error(
                    "CONVERSATION_READ_FAILED",
                    "materialize_events",
                    source.to_string(),
                )
            })?;
        let persisted = events
            .into_iter()
            .filter_map(|event| {
                legacy_event_type(event.type_).map(|type_| PersistedEventRecord {
                    schema_version: SESSION_SCHEMA_VERSION,
                    session_id: agent_session_id.to_string(),
                    seq: event.seq,
                    type_: type_.to_string(),
                    recorded_at: event.recorded_at_utc.timestamp_millis().max(0) as u64,
                    payload: event.payload,
                })
            })
            .collect::<Vec<_>>();
        let created_at = record.created_at_utc.timestamp_millis().max(0) as u64;
        Ok((
            SessionMetadata {
                schema_version: SESSION_SCHEMA_VERSION,
                storage_key: conversation_id.to_string(),
                session_id: agent_session_id.to_string(),
                stable_agent_namespace: Some(binding.stable_agent_namespace),
                runtime_agent_id: Some(binding.runtime_agent_id),
                project_id: record
                    .project_attachment
                    .as_ref()
                    .map(|attachment| attachment.project_id.clone()),
                cwd: binding.execution_cwd,
                title: summary.title,
                title_source: summary.title_source.map(acp_title_source),
                created_at,
                last_activity_at: summary.last_activity_at_utc.timestamp_millis().max(0) as u64,
                status: history_status(record.lifecycle_state, binding.state),
                message_count: summary.message_count,
                tool_count: summary.tool_count,
                last_seq: record.last_seq,
                discovered: false,
                worktree_path: record
                    .project_attachment
                    .as_ref()
                    .and_then(|attachment| attachment.worktree_path.clone()),
                worktree_branch: record
                    .project_attachment
                    .as_ref()
                    .and_then(|attachment| attachment.worktree_branch.clone()),
            },
            persisted,
        ))
    }

    pub fn last_seq(&self, agent_session_id: &str) -> Result<u64> {
        let conversation_id = self
            .conversation_id_for_history_binding(agent_session_id)
            .ok_or_else(|| error("CONVERSATION_NOT_FOUND", "last_seq", "binding not found"))?;
        self.reader
            .get(conversation_id)
            .map(|record| record.last_seq)
            .map_err(|source| error("CONVERSATION_READ_FAILED", "last_seq", source.to_string()))
    }

    pub fn replay_after(
        &self,
        agent_session_id: &str,
        cursor: u64,
    ) -> Result<Vec<PersistedEventRecord>> {
        let (_, records) = self.legacy_materialization(agent_session_id)?;
        Ok(records
            .into_iter()
            .filter(|record| record.seq > cursor)
            .collect())
    }

    pub async fn flush_all(&self) -> Result<()> {
        // Repository appends cross their durability boundary before returning; there is no
        // background writer queue to drain.
        Ok(())
    }
}

fn canonical_event_type(value: &str) -> Option<ConversationEventType> {
    match value {
        "user_prompt" => Some(ConversationEventType::UserPrompt),
        "message_chunk" => Some(ConversationEventType::MessageChunk),
        "session_info_update" => Some(ConversationEventType::SessionInfoUpdate),
        "local_title_generated" => Some(ConversationEventType::LocalTitleGenerated),
        "prompt_complete" => Some(ConversationEventType::PromptComplete),
        "tool_call" => Some(ConversationEventType::ToolCall),
        "tool_call_update" => Some(ConversationEventType::ToolCallUpdate),
        // Transport/lifecycle/config events remain live-only until their explicit Conversation
        // lifecycle mapping lands. They must not be guessed into a canonical stream.
        _ => None,
    }
}

fn legacy_event_type(value: ConversationEventType) -> Option<&'static str> {
    match value {
        ConversationEventType::UserPrompt => Some("user_prompt"),
        ConversationEventType::MessageChunk => Some("message_chunk"),
        ConversationEventType::SessionInfoUpdate => Some("session_info_update"),
        ConversationEventType::LocalTitleGenerated => Some("local_title_generated"),
        ConversationEventType::PromptComplete => Some("prompt_complete"),
        ConversationEventType::ToolCall => Some("tool_call"),
        ConversationEventType::ToolCallUpdate => Some("tool_call_update"),
        _ => None,
    }
}

fn acp_title_source(source: ConversationTitleSource) -> TitleSource {
    match source {
        ConversationTitleSource::BackgroundGenerated => TitleSource::BackgroundGenerated,
        ConversationTitleSource::AgentSupplied => TitleSource::AgentSupplied,
        ConversationTitleSource::DerivedFirstMessage => TitleSource::DerivedFirstMessage,
        ConversationTitleSource::LocalAlias => TitleSource::LocalAlias,
    }
}

fn history_status(
    lifecycle_state: ConversationLifecycleState,
    binding_state: AgentSessionBindingState,
) -> PersistedSessionStatus {
    match binding_state {
        AgentSessionBindingState::Active
            if lifecycle_state == ConversationLifecycleState::Ready =>
        {
            PersistedSessionStatus::Active
        }
        AgentSessionBindingState::Detached | AgentSessionBindingState::Suspended => {
            PersistedSessionStatus::Closed
        }
        AgentSessionBindingState::Active | AgentSessionBindingState::Replaced => {
            PersistedSessionStatus::Error
        }
    }
}

fn error(
    code: &'static str,
    operation: &'static str,
    detail: impl Into<String>,
) -> ConversationPersistenceError {
    ConversationPersistenceError {
        code,
        operation,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::contracts::{
        AgentSessionBinding, AgentSessionBindingState, ConversationCreator,
        ConversationLifecycleState, ConversationRecordV2, CreationPartition, ExecutionTarget,
        AGENT_SESSION_BINDING_SCHEMA_VERSION, CONVERSATION_SCHEMA_VERSION,
    };
    use crate::conversation::creation::ConversationCreationService;
    use crate::conversation::locator::{ConversationLocator, SessionWorkspaceLocator};
    use chrono::TimeZone;
    use uuid::Uuid;

    #[tokio::test]
    async fn resolves_binding_and_writes_only_the_conversation_repository() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let private = root.join("private");
        let visible = root.join("visible");
        std::fs::create_dir_all(&visible).unwrap();
        let (repository, _) = ConversationRepository::open(private.clone()).unwrap();
        let id = crate::conversation::ConversationId::parse("11111111-1111-4111-8111-111111111111")
            .unwrap();
        let created_at = Utc
            .timestamp_millis_opt(1_766_000_000_000)
            .single()
            .unwrap();
        repository
            .create_conversation(ConversationRecordV2 {
                schema_version: CONVERSATION_SCHEMA_VERSION,
                conversation_id: id,
                created_at_utc: created_at,
                creation_partition: CreationPartition::from_created_at(created_at),
                workspace_cwd: visible.join("workspace").to_string_lossy().into_owned(),
                execution_target: ExecutionTarget::Workspace,
                project_attachment: None,
                lifecycle_state: ConversationLifecycleState::InitializingAgent,
                last_seq: 0,
                created_by: ConversationCreator::Termul,
            })
            .await
            .unwrap();
        repository
            .bind_agent_session(
                id,
                AgentSessionBinding {
                    schema_version: AGENT_SESSION_BINDING_SCHEMA_VERSION,
                    binding_id: Uuid::new_v4(),
                    agent_session_id: "opaque/session".to_string(),
                    runtime_agent_id: "runtime".to_string(),
                    stable_agent_namespace: "stable".to_string(),
                    execution_cwd: visible.to_string_lossy().into_owned(),
                    bound_at_utc: created_at,
                    state: AgentSessionBindingState::Active,
                },
                created_at,
            )
            .await
            .unwrap();
        let reader = Arc::new(crate::conversation::ConversationReader::new(
            Arc::clone(&repository),
            crate::conversation::LegacyConversationReader::default(),
            crate::conversation::ReaderPrecedence::ConversationV2Only,
        ));
        let adapter = Arc::new(ConversationPersistenceAdapter::new(
            Arc::clone(&repository),
            reader,
        ));
        let seq = adapter
            .append_acp_event(
                "opaque/session",
                "message_chunk",
                serde_json::json!({"role":"agent","content":{"type":"text","text":"ok"}}),
            )
            .await
            .unwrap();
        assert_eq!(seq, 2);
        assert_eq!(repository.read_events(id, 0).unwrap().len(), 2);
        assert_eq!(adapter.list_sessions().len(), 1);
        let (metadata, events) = adapter.legacy_materialization("opaque/session").unwrap();
        assert_eq!(metadata.session_id, "opaque/session");
        assert_eq!(events.len(), 1);
        adapter.flush_all().await.unwrap();
        for legacy_root in ["acp-sessions", "acp-chat-history", "workspace-manifests"] {
            assert!(!temp.path().join(legacy_root).exists());
        }
        let _ = ConversationCreationService::new(
            repository,
            ConversationLocator::new(private).unwrap(),
            SessionWorkspaceLocator::new(visible).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn history_summary_and_detached_reads_survive_restart_while_writes_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let private = root.join("private");
        let visible = root.join("visible");
        std::fs::create_dir_all(&visible).unwrap();
        let (repository, _) = ConversationRepository::open(private.clone()).unwrap();
        let id = ConversationId::parse("22222222-2222-4222-8222-222222222222").unwrap();
        let created_at = Utc
            .timestamp_millis_opt(1_766_000_000_000)
            .single()
            .unwrap();
        repository
            .create_conversation(ConversationRecordV2 {
                schema_version: CONVERSATION_SCHEMA_VERSION,
                conversation_id: id,
                created_at_utc: created_at,
                creation_partition: CreationPartition::from_created_at(created_at),
                workspace_cwd: visible.join("workspace").to_string_lossy().into_owned(),
                execution_target: ExecutionTarget::Workspace,
                project_attachment: None,
                lifecycle_state: ConversationLifecycleState::InitializingAgent,
                last_seq: 0,
                created_by: ConversationCreator::Termul,
            })
            .await
            .unwrap();
        repository
            .bind_agent_session(
                id,
                AgentSessionBinding {
                    schema_version: AGENT_SESSION_BINDING_SCHEMA_VERSION,
                    binding_id: Uuid::new_v4(),
                    agent_session_id: "opaque/detached".to_string(),
                    runtime_agent_id: "runtime".to_string(),
                    stable_agent_namespace: "stable".to_string(),
                    execution_cwd: visible.to_string_lossy().into_owned(),
                    bound_at_utc: created_at,
                    state: AgentSessionBindingState::Active,
                },
                created_at,
            )
            .await
            .unwrap();
        for (type_, payload) in [
            (
                ConversationEventType::UserPrompt,
                serde_json::json!({"content":[{"type":"text","text":"Derived"}]}),
            ),
            (
                ConversationEventType::SessionInfoUpdate,
                serde_json::json!({"title":"Agent"}),
            ),
            (
                ConversationEventType::LocalTitleGenerated,
                serde_json::json!({"title":"Background"}),
            ),
            (
                ConversationEventType::SessionInfoUpdate,
                serde_json::json!({"title":"Ignored"}),
            ),
            (
                ConversationEventType::ToolCall,
                serde_json::json!({"toolCall":{"id":"one"}}),
            ),
            (
                ConversationEventType::ToolCallUpdate,
                serde_json::json!({"update":{"id":"one"}}),
            ),
        ] {
            repository
                .append_event(id, created_at, type_, payload)
                .await
                .unwrap();
        }
        repository
            .detach_agent_binding(id, created_at)
            .await
            .unwrap();

        let reader = Arc::new(crate::conversation::ConversationReader::new(
            Arc::clone(&repository),
            crate::conversation::LegacyConversationReader::default(),
            crate::conversation::ReaderPrecedence::ConversationV2Only,
        ));
        let adapter = Arc::new(ConversationPersistenceAdapter::new(
            Arc::clone(&repository),
            reader,
        ));
        assert!(adapter
            .conversation_id_for_active_binding("opaque/detached")
            .is_none());
        assert_eq!(
            adapter.conversation_id_for_history_binding("opaque/detached"),
            Some(id)
        );
        let sessions = adapter.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title.as_deref(), Some("Background"));
        assert_eq!(
            sessions[0].title_source,
            Some(TitleSource::BackgroundGenerated)
        );
        assert_eq!(sessions[0].message_count, 4);
        assert_eq!(sessions[0].tool_count, 2);
        assert_eq!(sessions[0].status, PersistedSessionStatus::Closed);
        let (metadata, events) = adapter.legacy_materialization("opaque/detached").unwrap();
        assert_eq!(metadata.title, sessions[0].title);
        assert_eq!(events.len(), 6);
        assert!(adapter
            .append_acp_event("opaque/detached", "message_chunk", serde_json::json!({}))
            .await
            .is_err());

        repository
            .rebind_detached_binding(id, created_at)
            .await
            .unwrap();
        repository
            .suspend_agent_binding(id, true, created_at)
            .await
            .unwrap();
        assert_eq!(
            repository.current_binding(id).unwrap().unwrap().state,
            AgentSessionBindingState::Suspended
        );
        assert_eq!(
            adapter.conversation_id_for_history_binding("opaque/detached"),
            Some(id)
        );
        assert_eq!(
            adapter.list_sessions()[0].status,
            PersistedSessionStatus::Closed
        );
        assert!(adapter
            .append_acp_event("opaque/detached", "message_chunk", serde_json::json!({}))
            .await
            .is_err());

        drop(adapter);
        drop(repository);
        let (repository, _) = ConversationRepository::open(private).unwrap();
        let reader = Arc::new(crate::conversation::ConversationReader::new(
            Arc::clone(&repository),
            crate::conversation::LegacyConversationReader::default(),
            crate::conversation::ReaderPrecedence::ConversationV2Only,
        ));
        let reopened = ConversationPersistenceAdapter::new(repository, reader);
        let sessions = reopened.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title.as_deref(), Some("Background"));
        assert_eq!(sessions[0].message_count, 4);
        assert_eq!(sessions[0].tool_count, 2);
        assert_eq!(sessions[0].status, PersistedSessionStatus::Closed);
    }

    #[tokio::test]
    async fn unmapped_events_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().canonicalize().unwrap().join("private");
        let (repository, _) = ConversationRepository::open(private).unwrap();
        let reader = Arc::new(crate::conversation::ConversationReader::new(
            Arc::clone(&repository),
            crate::conversation::LegacyConversationReader::default(),
            crate::conversation::ReaderPrecedence::ConversationV2Only,
        ));
        let adapter = Arc::new(ConversationPersistenceAdapter::new(repository, reader));
        let error = adapter
            .append_acp_event("unknown", "message_chunk", serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "CONVERSATION_BINDING_NOT_FOUND");
    }
}
