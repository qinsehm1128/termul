//! Durable host-side Conversation creation and ACP binding gate.
//!
//! Creation owns the ordering boundary: one immutable Conversation identity/time pair is written
//! canonically, the independent visible SessionWorkspace is made durable, metadata advances to
//! `initializing_agent`, and only then may an ACP creator run. Once canonical metadata exists this
//! service never rolls either root back and never touches PTYs, projects, or worktrees.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex as ParkingMutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use crate::conversation::contracts::{
    format_created_at_utc, AgentSessionBinding, AgentSessionBindingState, ConversationCreator,
    ConversationErrorCode, ConversationId, ConversationLifecycleState, ConversationRecordV2,
    CreationPartition, ExecutionTarget, ProjectAttachment, AGENT_SESSION_BINDING_SCHEMA_VERSION,
    CONVERSATION_SCHEMA_VERSION,
};
use crate::conversation::durable_fs::{DirectoryPermissions, DurableFileSystem, DurableFsError};
use crate::conversation::event_log::{ConversationEventRecordV2, ConversationEventType};
use crate::conversation::locator::{ConversationLocator, LocatorError, SessionWorkspaceLocator};
use crate::conversation::repository::{
    ConversationMetadataUpdate, ConversationRepository, RepositoryError,
};

pub const PREPARE_CONVERSATION_SCHEMA_VERSION: u32 = 1;
pub const PREPARED_CONVERSATION_SCHEMA_VERSION: u32 = 1;

/// Request for a new Conversation or a retry of a retained failed creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareConversationRequest {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_attachment: Option<ProjectAttachment>,
    pub execution_target: ExecutionTarget,
}

impl PrepareConversationRequest {
    #[must_use]
    pub fn new(execution_target: ExecutionTarget) -> Self {
        Self {
            schema_version: PREPARE_CONVERSATION_SCHEMA_VERSION,
            conversation_id: None,
            project_attachment: None,
            execution_target,
        }
    }
}

/// Durable pre-ACP result. `executionCwd` may point at a project/worktree, but `workspaceCwd`
/// remains the independent visible SessionWorkspace for the lifetime of the Conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedConversation {
    pub schema_version: u32,
    pub conversation_id: ConversationId,
    pub created_at_utc: String,
    pub creation_partition: CreationPartition,
    pub workspace_cwd: String,
    pub execution_cwd: String,
    pub lifecycle_state: ConversationLifecycleState,
}

/// Opaque ACP values returned only after the durable preparation gate has opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentBindingResult {
    pub agent_session_id: String,
    pub runtime_agent_id: String,
    pub stable_agent_namespace: String,
}

/// Sanitized ACP creation failure supplied to [`ConversationCreationService::create_with_agent_gate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCreationFailure {
    pub code: String,
    pub message: String,
}

impl AgentCreationFailure {
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Injectable UTC source. Production calls `Utc::now()` only through [`SystemClock`].
pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Injectable UUID source used for canonical Conversation ids and binding-history ids.
pub trait ConversationIdGenerator: Send + Sync {
    fn generate(&self) -> ConversationId;
}

#[derive(Debug, Default)]
pub struct DefaultConversationIdGenerator;

impl ConversationIdGenerator for DefaultConversationIdGenerator {
    fn generate(&self) -> ConversationId {
        ConversationId::parse(&Uuid::new_v4().to_string())
            .expect("UUIDv4 is a valid ConversationId")
    }
}

#[derive(Debug)]
pub struct ConversationCreationError {
    pub code: ConversationErrorCode,
    pub operation: &'static str,
    pub conversation_id: Option<ConversationId>,
    pub detail: String,
}

impl fmt::Display for ConversationCreationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?} during {}", self.code, self.operation)?;
        if let Some(conversation_id) = self.conversation_id {
            write!(formatter, " for Conversation {conversation_id}")?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl std::error::Error for ConversationCreationError {}

pub type Result<T> = std::result::Result<T, ConversationCreationError>;

/// Reusable durable creation transaction shared by future desktop and standalone bootstrap paths.
pub struct ConversationCreationService {
    repository: Arc<ConversationRepository>,
    private_locator: ConversationLocator,
    workspace_locator: SessionWorkspaceLocator,
    durable_fs: DurableFileSystem,
    clock: Arc<dyn Clock>,
    id_generator: Arc<dyn ConversationIdGenerator>,
    creation_locks: ParkingMutex<HashMap<ConversationId, Arc<TokioMutex<()>>>>,
}

impl ConversationCreationService {
    pub fn new(
        repository: Arc<ConversationRepository>,
        private_locator: ConversationLocator,
        workspace_locator: SessionWorkspaceLocator,
    ) -> Result<Self> {
        Self::with_sources(
            repository,
            private_locator,
            workspace_locator,
            DurableFileSystem::new(),
            Arc::new(SystemClock),
            Arc::new(DefaultConversationIdGenerator),
        )
    }

    pub fn with_sources(
        repository: Arc<ConversationRepository>,
        private_locator: ConversationLocator,
        workspace_locator: SessionWorkspaceLocator,
        durable_fs: DurableFileSystem,
        clock: Arc<dyn Clock>,
        id_generator: Arc<dyn ConversationIdGenerator>,
    ) -> Result<Self> {
        if repository.root() != private_locator.root() {
            return Err(creation_error(
                ConversationErrorCode::ConversationPathEscape,
                "construct",
                None,
                "repository and private locator roots differ",
            ));
        }
        Ok(Self {
            repository,
            private_locator,
            workspace_locator,
            durable_fs,
            clock,
            id_generator,
            creation_locks: ParkingMutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn repository(&self) -> &Arc<ConversationRepository> {
        &self.repository
    }

    /// Resolve a replacement execution target against an existing Conversation without mutating
    /// its identity, workspace, attachment, metadata revision, or binding history. The lifecycle
    /// service holds the repository Conversation lock while calling this method and commits the
    /// replacement binding only after the provider creates the new opaque session.
    pub fn prepare_replacement(
        &self,
        request: &PrepareConversationRequest,
    ) -> Result<PreparedConversation> {
        validate_request_schema(request)?;
        let conversation_id = request.conversation_id.ok_or_else(|| {
            creation_error(
                ConversationErrorCode::ConversationInvalidId,
                "prepare_replacement",
                None,
                "replacement request requires conversationId",
            )
        })?;
        let record = self
            .repository
            .get_conversation(conversation_id)
            .map_err(map_repository_error)?;
        if matches!(
            record.lifecycle_state,
            ConversationLifecycleState::Deleted | ConversationLifecycleState::RecoveryRequired
        ) {
            return Err(creation_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "prepare_replacement",
                Some(conversation_id),
                "Conversation lifecycle does not admit replacement",
            ));
        }
        if request
            .project_attachment
            .as_ref()
            .is_some_and(|requested| record.project_attachment.as_ref() != Some(requested))
        {
            return Err(creation_error(
                ConversationErrorCode::ConversationCreateFailed,
                "prepare_replacement",
                Some(conversation_id),
                "replacement cannot change project attachment",
            ));
        }
        if request.execution_target != record.execution_target {
            return Err(creation_error(
                ConversationErrorCode::ConversationCreateFailed,
                "prepare_replacement",
                Some(conversation_id),
                "replacement executionTarget must match canonical Conversation metadata",
            ));
        }
        let workspace = self.canonical_workspace_for(&record)?;
        let execution_cwd = self.resolve_execution_cwd(
            &request.execution_target,
            &workspace,
            record.project_attachment.as_ref(),
        )?;
        Ok(prepared_from_record(&record, execution_cwd))
    }

    /// Prepare canonical metadata and the independent visible workspace without contacting ACP.
    ///
    /// New creation ordering is exact: durable `allocating_workspace` metadata, durable visible
    /// directory, one atomic metadata transition to `initializing_agent`, then return. A retry
    /// reloads the canonical record and never allocates replacement identity/time values.
    pub async fn prepare_conversation(
        self: &Arc<Self>,
        request: PrepareConversationRequest,
    ) -> Result<PreparedConversation> {
        validate_request_schema(&request)?;
        let requested_id = request.conversation_id;
        if let Some(conversation_id) = requested_id {
            let lock = self.creation_lock(conversation_id);
            let _guard = lock.lock().await;
            return self.prepare_conversation_locked(request).await;
        }

        // Allocate exactly once for a new Conversation. The generated id determines the lock and
        // both canonical paths; no retry or later binding step can replace it.
        let conversation_id = self.id_generator.generate();
        let created_at_utc = self.clock.now_utc();
        let lock = self.creation_lock(conversation_id);
        let _guard = lock.lock().await;
        self.prepare_new_locked(request, conversation_id, created_at_utc)
            .await
    }

    /// Run an injected ACP creator only after [`Self::prepare_conversation`] has crossed the
    /// durable gate. Success appends an opaque binding; failure appends `creation_failed` and
    /// retains both roots for retry.
    pub async fn create_with_agent_gate<F, Fut>(
        self: &Arc<Self>,
        request: PrepareConversationRequest,
        creator: F,
    ) -> Result<PreparedConversation>
    where
        F: FnOnce(PreparedConversation) -> Fut,
        Fut: Future<Output = std::result::Result<AgentBindingResult, AgentCreationFailure>>,
    {
        validate_request_schema(&request)?;
        let (conversation_id, allocated_at) = match request.conversation_id {
            Some(conversation_id) => (conversation_id, None),
            None => (self.id_generator.generate(), Some(self.clock.now_utc())),
        };
        let lock = self.creation_lock(conversation_id);
        let _guard = lock.lock().await;
        let prepared = match allocated_at {
            Some(created_at_utc) => {
                self.prepare_new_locked(request, conversation_id, created_at_utc)
                    .await?
            }
            None => self.prepare_conversation_locked(request).await?,
        };

        match creator(prepared.clone()).await {
            Ok(binding) => {
                let record = self
                    .complete_agent_binding_locked(conversation_id, binding)
                    .await?;
                Ok(prepared_from_record(
                    &record,
                    self.resolve_execution_cwd(
                        &record.execution_target,
                        Path::new(&record.workspace_cwd),
                        record.project_attachment.as_ref(),
                    )?,
                ))
            }
            Err(failure) => {
                let code = failure.code;
                let message = sanitize_failure_message(&failure.message);
                self.record_agent_creation_failure_locked(conversation_id, &code, &message)
                    .await?;
                Err(creation_error(
                    ConversationErrorCode::ConversationCreateFailed,
                    "create_with_agent_gate",
                    Some(conversation_id),
                    format!(
                        "agent creation failed with code {}: {}",
                        sanitize_failure_code(&code)?,
                        message
                    ),
                ))
            }
        }
    }

    /// Append a new or replacement opaque ACP binding and materialize lifecycle `ready`.
    pub async fn complete_agent_binding(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        binding: AgentBindingResult,
    ) -> Result<ConversationRecordV2> {
        let lock = self.creation_lock(conversation_id);
        let _guard = lock.lock().await;
        self.complete_agent_binding_locked(conversation_id, binding)
            .await
    }

    /// Append a retryable `creation_failed` event and materialize lifecycle `agent_failed`.
    pub async fn record_agent_creation_failure(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        code: &str,
        message: &str,
    ) -> Result<ConversationEventRecordV2> {
        let lock = self.creation_lock(conversation_id);
        let _guard = lock.lock().await;
        self.record_agent_creation_failure_locked(conversation_id, code, message)
            .await
    }

    /// Reconcile interrupted creation without assuming an ACP subprocess survived restart.
    /// Missing visible directories are recreated from canonical metadata, existing/non-empty
    /// workspaces are left untouched, and no binding or resource teardown is fabricated.
    pub async fn recover_incomplete_creations(self: &Arc<Self>) -> Result<usize> {
        let records = self.repository.list_conversations();
        let mut recovered = 0usize;
        for record in records {
            let workspace = self.canonical_workspace_for(&record)?;
            let workspace_missing = match fs::symlink_metadata(&workspace) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        return Err(creation_error(
                            ConversationErrorCode::ConversationRecoveryRequired,
                            "recover_incomplete_creations",
                            Some(record.conversation_id),
                            "canonical workspace path is not a regular directory",
                        ));
                    }
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => {
                    return Err(creation_error(
                        ConversationErrorCode::ConversationRecoveryRequired,
                        "recover_incomplete_creations",
                        Some(record.conversation_id),
                        error.to_string(),
                    ));
                }
            };
            let interrupted = matches!(
                record.lifecycle_state,
                ConversationLifecycleState::AllocatingWorkspace
                    | ConversationLifecycleState::InitializingAgent
            );
            // ConversationRepository::open closes stale `initializing_agent` to `agent_failed`
            // before this service is constructed. A missing workspace on agent_failed is therefore
            // also an interrupted-creation reconciliation case.
            if !(interrupted
                || record.lifecycle_state == ConversationLifecycleState::AgentFailed
                    && workspace_missing)
            {
                continue;
            }

            let lock = self.creation_lock(record.conversation_id);
            let _guard = lock.lock().await;
            if workspace_missing {
                self.durable_fs
                    .create_dir_durable(&workspace, DirectoryPermissions::Inherit)
                    .map_err(|error| {
                        durable_creation_error(
                            "recover_incomplete_creations",
                            record.conversation_id,
                            error,
                        )
                    })?;
                log::warn!(
                    "[conversation-creation] missing visible workspace recreated conversation_id={}",
                    record.conversation_id
                );
            }
            if record.lifecycle_state != ConversationLifecycleState::AgentFailed {
                self.repository
                    .update_metadata(
                        record.conversation_id,
                        ConversationMetadataUpdate {
                            lifecycle_state: Some(ConversationLifecycleState::AgentFailed),
                            execution_target: None,
                        },
                    )
                    .await
                    .map_err(map_repository_error)?;
            }
            self.repository
                .sync_conversation(record.conversation_id)
                .await
                .map_err(map_repository_error)?;
            log::warn!(
                "[conversation-creation] interrupted creation recovered conversation_id={}",
                record.conversation_id
            );
            recovered += 1;
        }
        Ok(recovered)
    }

    async fn prepare_new_locked(
        &self,
        request: PrepareConversationRequest,
        conversation_id: ConversationId,
        created_at_utc: DateTime<Utc>,
    ) -> Result<PreparedConversation> {
        log::info!(
            "[conversation-creation] prepare start conversation_id={} retry=false",
            conversation_id
        );
        let creation_partition = CreationPartition::from_created_at(created_at_utc);
        let private_directory = self
            .private_locator
            .private_dir(conversation_id, &creation_partition)
            .map_err(|error| {
                map_locator_error("prepare_conversation", Some(conversation_id), error)
            })?;
        if !private_directory.starts_with(self.repository.root()) {
            return Err(creation_error(
                ConversationErrorCode::ConversationPathEscape,
                "prepare_conversation",
                Some(conversation_id),
                "private canonical directory escaped repository root",
            ));
        }
        let workspace = self
            .workspace_locator
            .workspace_dir(conversation_id, &creation_partition)
            .map_err(|error| {
                map_locator_error("prepare_conversation", Some(conversation_id), error)
            })?;
        let workspace_cwd =
            path_to_utf8(&workspace, "prepare_conversation", Some(conversation_id))?;
        let execution_cwd = self.resolve_execution_cwd(
            &request.execution_target,
            &workspace,
            request.project_attachment.as_ref(),
        )?;

        let record = ConversationRecordV2 {
            schema_version: CONVERSATION_SCHEMA_VERSION,
            conversation_id,
            created_at_utc,
            creation_partition: creation_partition.clone(),
            workspace_cwd,
            execution_target: request.execution_target,
            // Project attribution is recorded through append-only attachment history below.
            project_attachment: None,
            lifecycle_state: ConversationLifecycleState::AllocatingWorkspace,
            last_seq: 0,
            created_by: ConversationCreator::Termul,
        };
        self.repository
            .create_conversation(record)
            .await
            .map_err(map_repository_error)?;

        if let Some(attachment) = request.project_attachment {
            self.repository
                .append_project_attachment(
                    conversation_id,
                    attachment.clone(),
                    attachment.attached_at_utc,
                )
                .await
                .map_err(map_repository_error)?;
        }

        // Canonical metadata is durable before the visible namespace is created.
        self.durable_fs
            .create_dir_durable(&workspace, DirectoryPermissions::Inherit)
            .map_err(|error| {
                durable_creation_error("prepare_conversation", conversation_id, error)
            })?;
        log::info!(
            "[conversation-creation] workspace directory ready conversation_id={}",
            conversation_id
        );

        let record = self
            .repository
            .update_metadata(
                conversation_id,
                ConversationMetadataUpdate {
                    lifecycle_state: Some(ConversationLifecycleState::InitializingAgent),
                    execution_target: None,
                },
            )
            .await
            .map_err(map_repository_error)?;
        self.repository
            .sync_conversation(conversation_id)
            .await
            .map_err(map_repository_error)?;
        log::info!(
            "[conversation-creation] prepare success conversation_id={} lifecycle=initializing_agent",
            conversation_id
        );
        Ok(prepared_from_record(&record, execution_cwd))
    }

    async fn prepare_conversation_locked(
        &self,
        request: PrepareConversationRequest,
    ) -> Result<PreparedConversation> {
        let conversation_id = request.conversation_id.ok_or_else(|| {
            creation_error(
                ConversationErrorCode::ConversationInvalidId,
                "prepare_conversation",
                None,
                "retry request is missing conversationId",
            )
        })?;
        log::info!(
            "[conversation-creation] prepare start conversation_id={} retry=true",
            conversation_id
        );
        let existing = self
            .repository
            .get_conversation(conversation_id)
            .map_err(map_repository_error)?;
        if !matches!(
            existing.lifecycle_state,
            ConversationLifecycleState::AllocatingWorkspace
                | ConversationLifecycleState::InitializingAgent
                | ConversationLifecycleState::AgentFailed
        ) {
            return Err(creation_error(
                ConversationErrorCode::ConversationCreateFailed,
                "prepare_conversation",
                Some(conversation_id),
                "Conversation lifecycle is not retryable",
            ));
        }
        let workspace = self.canonical_workspace_for(&existing)?;
        let effective_attachment = request
            .project_attachment
            .as_ref()
            .or(existing.project_attachment.as_ref());
        let execution_cwd = self.resolve_execution_cwd(
            &request.execution_target,
            &workspace,
            effective_attachment,
        )?;

        match (&existing.project_attachment, request.project_attachment) {
            (None, Some(attachment)) => {
                self.repository
                    .append_project_attachment(
                        conversation_id,
                        attachment.clone(),
                        attachment.attached_at_utc,
                    )
                    .await
                    .map_err(map_repository_error)?;
            }
            (Some(current), Some(requested)) if current != &requested => {
                return Err(creation_error(
                    ConversationErrorCode::ConversationCreateFailed,
                    "prepare_conversation",
                    Some(conversation_id),
                    "retry cannot replace an existing project attachment",
                ));
            }
            _ => {}
        }

        self.durable_fs
            .create_dir_durable(&workspace, DirectoryPermissions::Inherit)
            .map_err(|error| {
                durable_creation_error("prepare_conversation", conversation_id, error)
            })?;
        log::info!(
            "[conversation-creation] workspace directory ready conversation_id={}",
            conversation_id
        );
        let record = self
            .repository
            .update_metadata(
                conversation_id,
                ConversationMetadataUpdate {
                    lifecycle_state: Some(ConversationLifecycleState::InitializingAgent),
                    execution_target: Some(request.execution_target),
                },
            )
            .await
            .map_err(map_repository_error)?;
        self.repository
            .sync_conversation(conversation_id)
            .await
            .map_err(map_repository_error)?;
        log::info!(
            "[conversation-creation] retry using canonical conversation_id={} lifecycle=initializing_agent",
            conversation_id
        );
        Ok(prepared_from_record(&record, execution_cwd))
    }

    async fn complete_agent_binding_locked(
        &self,
        conversation_id: ConversationId,
        binding: AgentBindingResult,
    ) -> Result<ConversationRecordV2> {
        validate_binding_result(conversation_id, &binding)?;
        let record = self
            .repository
            .get_conversation(conversation_id)
            .map_err(map_repository_error)?;
        if !matches!(
            record.lifecycle_state,
            ConversationLifecycleState::InitializingAgent
                | ConversationLifecycleState::AgentFailed
                | ConversationLifecycleState::Ready
        ) {
            return Err(creation_error(
                ConversationErrorCode::ConversationBindFailed,
                "complete_agent_binding",
                Some(conversation_id),
                "Conversation lifecycle does not admit agent binding",
            ));
        }
        let workspace = self.canonical_workspace_for(&record)?;
        let execution_cwd = self.resolve_execution_cwd(
            &record.execution_target,
            &workspace,
            record.project_attachment.as_ref(),
        )?;
        let binding_id = *self.id_generator.generate().as_uuid();
        let bound_at_utc = self.clock.now_utc();
        let value = AgentSessionBinding {
            schema_version: AGENT_SESSION_BINDING_SCHEMA_VERSION,
            binding_id,
            agent_session_id: binding.agent_session_id,
            runtime_agent_id: binding.runtime_agent_id,
            stable_agent_namespace: binding.stable_agent_namespace,
            execution_cwd,
            bound_at_utc,
            state: AgentSessionBindingState::Active,
        };
        if self
            .repository
            .current_binding(conversation_id)
            .map_err(map_repository_error)?
            .is_some()
        {
            self.repository
                .replace_agent_binding(conversation_id, value, bound_at_utc)
                .await
                .map_err(map_repository_error)?;
        } else {
            self.repository
                .bind_agent_session(conversation_id, value, bound_at_utc)
                .await
                .map_err(map_repository_error)?;
        }
        self.repository
            .sync_conversation(conversation_id)
            .await
            .map_err(map_repository_error)?;
        let ready = self
            .repository
            .get_conversation(conversation_id)
            .map_err(map_repository_error)?;
        log::info!(
            "[conversation-creation] binding complete conversation_id={} lifecycle=ready",
            conversation_id
        );
        Ok(ready)
    }

    async fn record_agent_creation_failure_locked(
        &self,
        conversation_id: ConversationId,
        code: &str,
        message: &str,
    ) -> Result<ConversationEventRecordV2> {
        let code = sanitize_failure_code(code)?;
        let message = sanitize_failure_message(message);
        let record = self
            .repository
            .get_conversation(conversation_id)
            .map_err(map_repository_error)?;
        if !matches!(
            record.lifecycle_state,
            ConversationLifecycleState::AllocatingWorkspace
                | ConversationLifecycleState::InitializingAgent
                | ConversationLifecycleState::AgentFailed
        ) {
            return Err(creation_error(
                ConversationErrorCode::ConversationCreateFailed,
                "record_agent_creation_failure",
                Some(conversation_id),
                "Conversation lifecycle does not admit a creation failure",
            ));
        }
        let event = self
            .repository
            .append_event(
                conversation_id,
                self.clock.now_utc(),
                ConversationEventType::CreationFailed,
                serde_json::json!({
                    "code": code,
                    "retryable": true,
                    "message": message,
                }),
            )
            .await
            .map_err(map_repository_error)?;
        self.repository
            .sync_conversation(conversation_id)
            .await
            .map_err(map_repository_error)?;
        log::warn!(
            "[conversation-creation] ACP creation failed but Conversation retained conversation_id={} code={}",
            conversation_id,
            code
        );
        Ok(event)
    }

    fn canonical_workspace_for(&self, record: &ConversationRecordV2) -> Result<PathBuf> {
        let workspace = self
            .workspace_locator
            .workspace_dir(record.conversation_id, &record.creation_partition)
            .map_err(|error| {
                map_locator_error("canonical_workspace", Some(record.conversation_id), error)
            })?;
        let canonical = path_to_utf8(
            &workspace,
            "canonical_workspace",
            Some(record.conversation_id),
        )?;
        if canonical != record.workspace_cwd {
            return Err(creation_error(
                ConversationErrorCode::ConversationRecoveryRequired,
                "canonical_workspace",
                Some(record.conversation_id),
                "workspaceCwd does not match immutable locator output",
            ));
        }
        Ok(workspace)
    }

    fn resolve_execution_cwd(
        &self,
        target: &ExecutionTarget,
        workspace: &Path,
        attachment: Option<&ProjectAttachment>,
    ) -> Result<String> {
        match target {
            ExecutionTarget::Workspace => path_to_utf8(workspace, "resolve_execution_target", None),
            ExecutionTarget::ProjectRoot {
                project_id,
                project_root,
            } => {
                validate_project_context(project_id, attachment)?;
                resolve_existing_directory(project_root, "project_root")
            }
            ExecutionTarget::Worktree {
                project_id,
                worktree_path,
                worktree_branch,
            } => {
                validate_project_context(project_id, attachment)?;
                if worktree_branch.trim().is_empty() {
                    return Err(target_validation_error("worktree branch is empty"));
                }
                resolve_existing_directory(worktree_path, "worktree")
            }
        }
    }

    fn creation_lock(&self, conversation_id: ConversationId) -> Arc<TokioMutex<()>> {
        let mut locks = self.creation_locks.lock();
        Arc::clone(
            locks
                .entry(conversation_id)
                .or_insert_with(|| Arc::new(TokioMutex::new(()))),
        )
    }
}

fn prepared_from_record(
    record: &ConversationRecordV2,
    execution_cwd: String,
) -> PreparedConversation {
    PreparedConversation {
        schema_version: PREPARED_CONVERSATION_SCHEMA_VERSION,
        conversation_id: record.conversation_id,
        created_at_utc: format_created_at_utc(&record.created_at_utc),
        creation_partition: record.creation_partition.clone(),
        workspace_cwd: record.workspace_cwd.clone(),
        execution_cwd,
        lifecycle_state: record.lifecycle_state,
    }
}

fn validate_request_schema(request: &PrepareConversationRequest) -> Result<()> {
    if request.schema_version != PREPARE_CONVERSATION_SCHEMA_VERSION {
        return Err(creation_error(
            ConversationErrorCode::ConversationUnsupportedSchema,
            "prepare_conversation",
            request.conversation_id,
            "unsupported prepare request schemaVersion",
        ));
    }
    Ok(())
}

fn validate_binding_result(
    conversation_id: ConversationId,
    binding: &AgentBindingResult,
) -> Result<()> {
    if binding.agent_session_id.trim().is_empty()
        || binding.runtime_agent_id.trim().is_empty()
        || binding.stable_agent_namespace.trim().is_empty()
    {
        return Err(creation_error(
            ConversationErrorCode::ConversationBindFailed,
            "complete_agent_binding",
            Some(conversation_id),
            "agent binding result has an empty required opaque field",
        ));
    }
    Ok(())
}

fn validate_project_context(
    project_id: &str,
    attachment: Option<&ProjectAttachment>,
) -> Result<()> {
    if project_id.trim().is_empty() {
        return Err(target_validation_error(
            "execution target projectId is empty",
        ));
    }
    if attachment.is_some_and(|attachment| attachment.project_id != project_id) {
        return Err(target_validation_error(
            "execution target projectId does not match project attachment",
        ));
    }
    Ok(())
}

fn resolve_existing_directory(value: &str, kind: &'static str) -> Result<String> {
    let path = Path::new(value);
    if !path.is_absolute() {
        log::warn!(
            "[conversation-creation] explicit execution target rejected kind={} reason=relative",
            kind
        );
        return Err(target_validation_error(
            "explicit execution target must be an absolute path",
        ));
    }
    let resolved = fs::canonicalize(path).map_err(|error| {
        log::warn!(
            "[conversation-creation] explicit execution target unavailable kind={}",
            kind
        );
        creation_error(
            ConversationErrorCode::ConversationCreateFailed,
            "resolve_execution_target",
            None,
            error.to_string(),
        )
    })?;
    if !resolved.is_dir() {
        return Err(target_validation_error(
            "explicit execution target is not a directory",
        ));
    }
    path_to_utf8(&resolved, "resolve_execution_target", None)
}

fn path_to_utf8(
    path: &Path,
    operation: &'static str,
    conversation_id: Option<ConversationId>,
) -> Result<String> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        creation_error(
            ConversationErrorCode::ConversationCreateFailed,
            operation,
            conversation_id,
            "canonical path is not valid UTF-8",
        )
    })
}

fn sanitize_failure_code(code: &str) -> Result<String> {
    let code = code.trim();
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(creation_error(
            ConversationErrorCode::ConversationCreateFailed,
            "record_agent_creation_failure",
            None,
            "creation failure code must be bounded SCREAMING_SNAKE_CASE",
        ));
    }
    Ok(code.to_string())
}

fn sanitize_failure_message(message: &str) -> String {
    let sanitized = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(256)
        .collect::<String>();
    let sanitized = sanitized.trim();
    if sanitized.is_empty() {
        "Agent creation failed".to_string()
    } else {
        sanitized.to_string()
    }
}

fn target_validation_error(detail: &str) -> ConversationCreationError {
    creation_error(
        ConversationErrorCode::ConversationCreateFailed,
        "resolve_execution_target",
        None,
        detail,
    )
}

fn map_repository_error(error: RepositoryError) -> ConversationCreationError {
    creation_error(
        error.code,
        error.operation,
        error.conversation_id,
        error.detail,
    )
}

fn map_locator_error(
    operation: &'static str,
    conversation_id: Option<ConversationId>,
    error: LocatorError,
) -> ConversationCreationError {
    creation_error(
        ConversationErrorCode::ConversationPathEscape,
        operation,
        conversation_id,
        error.to_string(),
    )
}

fn durable_creation_error(
    operation: &'static str,
    conversation_id: ConversationId,
    error: DurableFsError,
) -> ConversationCreationError {
    creation_error(
        ConversationErrorCode::ConversationDurabilityFailed,
        operation,
        Some(conversation_id),
        error.to_string(),
    )
}

fn creation_error(
    code: ConversationErrorCode,
    operation: &'static str,
    conversation_id: Option<ConversationId>,
    detail: impl Into<String>,
) -> ConversationCreationError {
    log::error!(
        "[conversation-creation] operation failed code={:?} operation={} conversation_id={}",
        code,
        operation,
        conversation_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    ConversationCreationError {
        code,
        operation,
        conversation_id,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::catalog::CONVERSATION_METADATA_FILE;
    use crate::conversation::contracts::{parse_created_at_utc, PROJECT_ATTACHMENT_SCHEMA_VERSION};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    const ID: &str = "018f7a1c-1b4d-4c8a-9f01-0123456789ab";
    const BINDING_ID: &str = "b2832b54-2ca4-4db4-93fd-f93bf6793114";
    const REPLACEMENT_ID: &str = "c3943c65-3db5-4ec5-a4e0-0a4cf78a4225";

    struct FixedClock {
        values: ParkingMutex<VecDeque<DateTime<Utc>>>,
        calls: AtomicUsize,
    }

    impl FixedClock {
        fn new(values: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                values: ParkingMutex::new(
                    values
                        .iter()
                        .map(|value| parse_created_at_utc(value).unwrap())
                        .collect(),
                ),
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl Clock for FixedClock {
        fn now_utc(&self) -> DateTime<Utc> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.values
                .lock()
                .pop_front()
                .expect("fixed clock exhausted")
        }
    }

    struct FixedIds {
        values: ParkingMutex<VecDeque<ConversationId>>,
        calls: AtomicUsize,
    }

    impl FixedIds {
        fn new(values: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                values: ParkingMutex::new(
                    values
                        .iter()
                        .map(|value| ConversationId::parse(value).unwrap())
                        .collect(),
                ),
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl ConversationIdGenerator for FixedIds {
        fn generate(&self) -> ConversationId {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.values.lock().pop_front().expect("fixed ids exhausted")
        }
    }

    struct Fixture {
        _temp: TempDir,
        private_root: PathBuf,
        visible_root: PathBuf,
        repository: Arc<ConversationRepository>,
        service: Arc<ConversationCreationService>,
        clock: Arc<FixedClock>,
        ids: Arc<FixedIds>,
    }

    fn fixture(clock_values: &[&str], id_values: &[&str]) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let private_root = base.join("private");
        let visible_root = base.join("visible");
        fs::create_dir_all(&visible_root).unwrap();
        let (repository, report) = ConversationRepository::open(private_root.clone()).unwrap();
        assert_eq!(report.valid_conversation_count, 0);
        let clock = FixedClock::new(clock_values);
        let ids = FixedIds::new(id_values);
        let service = Arc::new(
            ConversationCreationService::with_sources(
                Arc::clone(&repository),
                ConversationLocator::new(private_root.clone()).unwrap(),
                SessionWorkspaceLocator::new(visible_root.clone()).unwrap(),
                DurableFileSystem::new(),
                clock.clone(),
                ids.clone(),
            )
            .unwrap(),
        );
        Fixture {
            _temp: temp,
            private_root,
            visible_root,
            repository,
            service,
            clock,
            ids,
        }
    }

    fn request(target: ExecutionTarget) -> PrepareConversationRequest {
        PrepareConversationRequest::new(target)
    }

    fn retry(
        conversation_id: ConversationId,
        target: ExecutionTarget,
    ) -> PrepareConversationRequest {
        PrepareConversationRequest {
            schema_version: PREPARE_CONVERSATION_SCHEMA_VERSION,
            conversation_id: Some(conversation_id),
            project_attachment: None,
            execution_target: target,
        }
    }

    fn binding(opaque: &str) -> AgentBindingResult {
        AgentBindingResult {
            agent_session_id: opaque.to_string(),
            runtime_agent_id: "runtime-1".to_string(),
            stable_agent_namespace: "config:test".to_string(),
        }
    }

    #[tokio::test]
    async fn project_less_prepare_allocates_one_identity_and_time_for_matching_paths() {
        let fixture = fixture(&["2026-08-15T09:45:15.123Z"], &[ID]);
        let prepared = fixture
            .service
            .prepare_conversation(request(ExecutionTarget::Workspace))
            .await
            .unwrap();
        assert_eq!(fixture.ids.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.clock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(prepared.conversation_id.to_string(), ID);
        assert_eq!(prepared.created_at_utc, "2026-08-15T09:45:15.123Z");
        assert_eq!(prepared.creation_partition.path, "2026/08/15");
        assert_eq!(prepared.workspace_cwd, prepared.execution_cwd);
        assert_eq!(
            PathBuf::from(&prepared.workspace_cwd),
            fixture.visible_root.join("sessions/2026/08/15").join(ID)
        );
        assert!(fixture
            .private_root
            .join("2026/08/15")
            .join(ID)
            .join(CONVERSATION_METADATA_FILE)
            .is_file());
        assert!(Path::new(&prepared.workspace_cwd).is_dir());
        assert_eq!(
            prepared.lifecycle_state,
            ConversationLifecycleState::InitializingAgent
        );
    }

    #[tokio::test]
    async fn workspace_exists_before_acp_gate() {
        let fixture = fixture(
            &["2026-08-15T09:45:15.123Z", "2026-08-15T09:45:16.000Z"],
            &[ID, BINDING_ID],
        );
        let private_root = fixture.private_root.clone();
        let visible_root = fixture.visible_root.clone();
        let ids = fixture.ids.clone();
        let clock = fixture.clock.clone();
        let prepared = fixture
            .service
            .create_with_agent_gate(
                request(ExecutionTarget::Workspace),
                move |prepared| async move {
                    assert_eq!(ids.calls.load(Ordering::SeqCst), 1);
                    assert_eq!(clock.calls.load(Ordering::SeqCst), 1);
                    let metadata = private_root
                        .join("2026/08/15")
                        .join(ID)
                        .join(CONVERSATION_METADATA_FILE);
                    assert!(metadata.is_file(), "canonical metadata precedes ACP");
                    let decoded: ConversationRecordV2 =
                        serde_json::from_slice(&fs::read(metadata).unwrap()).unwrap();
                    assert_eq!(
                        decoded.lifecycle_state,
                        ConversationLifecycleState::InitializingAgent
                    );
                    assert_eq!(decoded.workspace_cwd, prepared.workspace_cwd);
                    assert_eq!(
                        PathBuf::from(&prepared.workspace_cwd),
                        visible_root.join("sessions/2026/08/15").join(ID)
                    );
                    assert!(Path::new(&prepared.workspace_cwd).is_dir());
                    Ok(binding("provider/session:not-a-uuid"))
                },
            )
            .await
            .unwrap();
        assert_eq!(prepared.lifecycle_state, ConversationLifecycleState::Ready);
        assert_eq!(
            fixture
                .repository
                .current_binding(prepared.conversation_id)
                .unwrap()
                .unwrap()
                .agent_session_id,
            "provider/session:not-a-uuid"
        );
    }

    #[tokio::test]
    async fn explicit_targets_and_attachment_never_change_identity_partition_or_workspace() {
        let fixture = fixture(&["2026-08-15T09:45:15.123Z"], &[ID]);
        let project = fixture._temp.path().join("project");
        let worktree = fixture._temp.path().join("worktree");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        let attachment = ProjectAttachment {
            schema_version: PROJECT_ATTACHMENT_SCHEMA_VERSION,
            project_id: "project-1".to_string(),
            attached_at_utc: parse_created_at_utc("2026-08-15T09:45:15.500Z").unwrap(),
            project_path_snapshot: project.to_string_lossy().into_owned(),
            worktree_path: None,
            worktree_branch: None,
        };
        let mut initial = request(ExecutionTarget::ProjectRoot {
            project_id: "project-1".to_string(),
            project_root: project.to_string_lossy().into_owned(),
        });
        initial.project_attachment = Some(attachment);
        let first = fixture.service.prepare_conversation(initial).await.unwrap();
        let identity = (
            first.conversation_id,
            first.created_at_utc.clone(),
            first.creation_partition.clone(),
            first.workspace_cwd.clone(),
        );
        let second = fixture
            .service
            .prepare_conversation(retry(
                first.conversation_id,
                ExecutionTarget::Worktree {
                    project_id: "project-1".to_string(),
                    worktree_path: worktree.to_string_lossy().into_owned(),
                    worktree_branch: "chat/example".to_string(),
                },
            ))
            .await
            .unwrap();
        assert_eq!(
            (
                second.conversation_id,
                second.created_at_utc,
                second.creation_partition,
                second.workspace_cwd.clone(),
            ),
            identity
        );
        assert_eq!(
            second.execution_cwd,
            worktree.canonicalize().unwrap().to_str().unwrap()
        );
        assert!(Path::new(&second.workspace_cwd).is_dir());
        assert_eq!(fixture.ids.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.clock.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn acp_gate_failure_records_retryable_state_without_deleting_roots() {
        let fixture = fixture(
            &["2026-08-15T09:45:15.123Z", "2026-08-15T09:45:16.000Z"],
            &[ID],
        );
        let result = fixture
            .service
            .create_with_agent_gate(request(ExecutionTarget::Workspace), |prepared| async move {
                assert!(Path::new(&prepared.workspace_cwd).is_dir());
                Err(AgentCreationFailure::new(
                    "ACP_SESSION_NEW_FAILED",
                    "provider unavailable",
                ))
            })
            .await;
        assert!(result.is_err());
        let conversation_id = ConversationId::parse(ID).unwrap();
        let record = fixture
            .repository
            .get_conversation(conversation_id)
            .unwrap();
        assert_eq!(
            record.lifecycle_state,
            ConversationLifecycleState::AgentFailed
        );
        assert!(fixture
            .repository
            .current_binding(conversation_id)
            .unwrap()
            .is_none());
        assert!(fixture.private_root.join("2026/08/15").join(ID).is_dir());
        assert!(Path::new(&record.workspace_cwd).is_dir());
        let failure = fixture
            .repository
            .read_events(conversation_id, 0)
            .unwrap()
            .into_iter()
            .find(|event| event.type_ == ConversationEventType::CreationFailed)
            .unwrap();
        assert_eq!(failure.payload["code"], "ACP_SESSION_NEW_FAILED");
        assert_eq!(failure.payload["retryable"], true);
        assert_eq!(failure.payload["message"], "provider unavailable");
        assert_eq!(fixture.ids.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.clock.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retry_preserves_identity_and_cwd() {
        let fixture = fixture(
            &["2026-08-15T09:45:15.123Z", "2026-08-15T09:45:16.000Z"],
            &[ID],
        );
        let prepared = fixture
            .service
            .prepare_conversation(request(ExecutionTarget::Workspace))
            .await
            .unwrap();
        let workspace = PathBuf::from(&prepared.workspace_cwd);
        fs::write(workspace.join("user-file.txt"), b"retain me").unwrap();
        fixture
            .service
            .record_agent_creation_failure(
                prepared.conversation_id,
                "ACP_SESSION_NEW_FAILED",
                "provider unavailable",
            )
            .await
            .unwrap();
        let failed = fixture
            .repository
            .get_conversation(prepared.conversation_id)
            .unwrap();
        assert_eq!(
            failed.lifecycle_state,
            ConversationLifecycleState::AgentFailed
        );
        assert!(fixture
            .repository
            .current_binding(prepared.conversation_id)
            .unwrap()
            .is_none());
        assert!(fixture.private_root.join("2026/08/15").join(ID).is_dir());
        assert_eq!(
            fs::read(workspace.join("user-file.txt")).unwrap(),
            b"retain me"
        );

        let retried = fixture
            .service
            .prepare_conversation(retry(prepared.conversation_id, ExecutionTarget::Workspace))
            .await
            .unwrap();
        assert_eq!(retried.conversation_id, prepared.conversation_id);
        assert_eq!(retried.created_at_utc, prepared.created_at_utc);
        assert_eq!(retried.creation_partition, prepared.creation_partition);
        assert_eq!(retried.workspace_cwd, prepared.workspace_cwd);
        assert_eq!(
            fs::read(workspace.join("user-file.txt")).unwrap(),
            b"retain me"
        );
        assert_eq!(fixture.ids.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.clock.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn opaque_binding_success_and_replacement_never_rename_canonical_directory() {
        let fixture = fixture(
            &[
                "2026-08-15T09:45:15.123Z",
                "2026-08-15T09:45:16.000Z",
                "2026-08-15T09:45:17.000Z",
            ],
            &[ID, BINDING_ID, REPLACEMENT_ID],
        );
        let prepared = fixture
            .service
            .prepare_conversation(request(ExecutionTarget::Workspace))
            .await
            .unwrap();
        let canonical = fixture.private_root.join("2026/08/15").join(ID);
        fixture
            .service
            .complete_agent_binding(
                prepared.conversation_id,
                binding("provider/session:opaque?generation=1"),
            )
            .await
            .unwrap();
        fixture
            .service
            .complete_agent_binding(
                prepared.conversation_id,
                binding("provider/session:opaque?generation=2"),
            )
            .await
            .unwrap();
        assert!(canonical.is_dir());
        assert!(!fixture
            .private_root
            .join("2026/08/15/provider/session:opaque?generation=2")
            .exists());
        let record = fixture
            .repository
            .get_conversation(prepared.conversation_id)
            .unwrap();
        assert_eq!(record.lifecycle_state, ConversationLifecycleState::Ready);
        assert_eq!(record.workspace_cwd, prepared.workspace_cwd);
        let history = fixture
            .repository
            .binding_history(prepared.conversation_id)
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].state, AgentSessionBindingState::Replaced);
        assert_eq!(
            history[1].agent_session_id,
            "provider/session:opaque?generation=2"
        );
    }

    #[tokio::test]
    async fn startup_recovery_preserves_nonempty_workspace_and_recreates_only_missing_workspace() {
        let fixture = fixture(&["2026-08-15T09:45:15.123Z"], &[ID]);
        let prepared = fixture
            .service
            .prepare_conversation(request(ExecutionTarget::Workspace))
            .await
            .unwrap();
        let workspace = PathBuf::from(&prepared.workspace_cwd);
        fs::write(workspace.join("user-data.txt"), b"never delete").unwrap();
        fixture
            .repository
            .update_metadata(
                prepared.conversation_id,
                ConversationMetadataUpdate {
                    lifecycle_state: Some(ConversationLifecycleState::AllocatingWorkspace),
                    execution_target: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            fixture
                .service
                .recover_incomplete_creations()
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            fs::read(workspace.join("user-data.txt")).unwrap(),
            b"never delete"
        );
        assert_eq!(
            fixture
                .repository
                .get_conversation(prepared.conversation_id)
                .unwrap()
                .lifecycle_state,
            ConversationLifecycleState::AgentFailed
        );

        fs::remove_file(workspace.join("user-data.txt")).unwrap();
        fs::remove_dir(&workspace).unwrap();
        assert_eq!(
            fixture
                .service
                .recover_incomplete_creations()
                .await
                .unwrap(),
            1
        );
        assert!(workspace.is_dir());
        assert!(fs::read_dir(&workspace).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn reopening_repository_then_recovery_recreates_missing_initializing_workspace() {
        let fixture = fixture(&["2026-08-15T09:45:15.123Z"], &[ID]);
        let prepared = fixture
            .service
            .prepare_conversation(request(ExecutionTarget::Workspace))
            .await
            .unwrap();
        let workspace = PathBuf::from(&prepared.workspace_cwd);
        fs::remove_dir(&workspace).unwrap();
        let (reopened, _) = ConversationRepository::open(fixture.private_root.clone()).unwrap();
        assert_eq!(
            reopened
                .get_conversation(prepared.conversation_id)
                .unwrap()
                .lifecycle_state,
            ConversationLifecycleState::AgentFailed
        );
        let service = Arc::new(
            ConversationCreationService::with_sources(
                Arc::clone(&reopened),
                ConversationLocator::new(fixture.private_root.clone()).unwrap(),
                SessionWorkspaceLocator::new(fixture.visible_root.clone()).unwrap(),
                DurableFileSystem::new(),
                fixture.clock.clone(),
                fixture.ids.clone(),
            )
            .unwrap(),
        );
        assert_eq!(service.recover_incomplete_creations().await.unwrap(), 1);
        assert!(workspace.is_dir());
        assert!(reopened
            .current_binding(prepared.conversation_id)
            .unwrap()
            .is_none());
    }
}
