//! Canonical v2 Conversation JSONL routing, validation, replay, and materialization.
//!
//! Every Conversation owns one global monotonically increasing sequence across four physical
//! streams. Stream files are append-only; replay validates their physical order, merges them by
//! `seq`, and rejects duplicate or mismatched records. Recovery repairs only an unterminated final
//! line, preserving the original bytes beside the log before atomically truncating it.

#[cfg(test)]
use std::cell::Cell;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

use crate::conversation::contracts::{
    format_created_at_utc, parse_created_at_utc, AgentSessionBinding, AgentSessionBindingState,
    ConversationErrorCode, ConversationId, ConversationTitleSource, ProjectAttachment,
    AGENT_SESSION_BINDING_SCHEMA_VERSION, PROJECT_ATTACHMENT_SCHEMA_VERSION,
};
use crate::conversation::durable_fs::DurableFileSystem;

pub const CONVERSATION_EVENT_SCHEMA_VERSION: u32 = 2;
pub const MESSAGES_FILE: &str = "messages.jsonl";
pub const TOOL_CALLS_FILE: &str = "tool-calls.jsonl";
pub const BINDINGS_FILE: &str = "bindings.jsonl";
pub const ATTACHMENTS_FILE: &str = "attachments.jsonl";
pub const EVENT_LOG_FILES: [&str; 4] = [
    MESSAGES_FILE,
    TOOL_CALLS_FILE,
    BINDINGS_FILE,
    ATTACHMENTS_FILE,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationEventStream {
    Messages,
    ToolCalls,
    Bindings,
    Attachments,
}

impl ConversationEventStream {
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Messages => MESSAGES_FILE,
            Self::ToolCalls => TOOL_CALLS_FILE,
            Self::Bindings => BINDINGS_FILE,
            Self::Attachments => ATTACHMENTS_FILE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationEventType {
    UserPrompt,
    MessageChunk,
    SessionInfoUpdate,
    LocalTitleGenerated,
    PromptComplete,
    ToolCall,
    ToolCallUpdate,
    BindingBound,
    BindingDetached,
    BindingRebound,
    BindingSuspended,
    BindingReplaced,
    ProjectAttached,
    ProjectDetached,
    CreationFailed,
}

impl ConversationEventType {
    #[must_use]
    pub const fn stream(self) -> ConversationEventStream {
        match self {
            Self::ToolCall | Self::ToolCallUpdate => ConversationEventStream::ToolCalls,
            Self::BindingBound
            | Self::BindingDetached
            | Self::BindingRebound
            | Self::BindingSuspended
            | Self::BindingReplaced => ConversationEventStream::Bindings,
            Self::ProjectAttached | Self::ProjectDetached => ConversationEventStream::Attachments,
            Self::UserPrompt
            | Self::MessageChunk
            | Self::SessionInfoUpdate
            | Self::LocalTitleGenerated
            | Self::PromptComplete
            | Self::CreationFailed => ConversationEventStream::Messages,
        }
    }
}

fn serialize_utc_millis<S>(
    value: &DateTime<Utc>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format_created_at_utc(value))
}

fn deserialize_utc_millis<'de, D>(deserializer: D) -> std::result::Result<DateTime<Utc>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    parse_created_at_utc(&value).map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationEventRecordV2 {
    pub schema_version: u32,
    pub conversation_id: ConversationId,
    pub seq: u64,
    #[serde(
        serialize_with = "serialize_utc_millis",
        deserialize_with = "deserialize_utc_millis"
    )]
    pub recorded_at_utc: DateTime<Utc>,
    #[serde(rename = "type")]
    pub type_: ConversationEventType,
    pub payload: Value,
}

impl ConversationEventRecordV2 {
    #[must_use]
    pub fn new(
        conversation_id: ConversationId,
        seq: u64,
        recorded_at_utc: DateTime<Utc>,
        type_: ConversationEventType,
        payload: Value,
    ) -> Self {
        Self {
            schema_version: CONVERSATION_EVENT_SCHEMA_VERSION,
            conversation_id,
            seq,
            recorded_at_utc,
            type_,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingEventPayloadV1 {
    pub binding: AgentSessionBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingReplacementPayloadV1 {
    pub previous_binding: AgentSessionBinding,
    pub binding: AgentSessionBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectAttachmentEventPayloadV1 {
    pub attachment: ProjectAttachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventLogRepairWarning {
    pub conversation_id: ConversationId,
    pub stream: String,
    pub backup_file: String,
    pub truncated_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationSummaryFrontier {
    pub title: Option<String>,
    pub title_source: Option<ConversationTitleSource>,
    pub last_activity_at_utc: Option<DateTime<Utc>>,
    pub message_count: u64,
    pub tool_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationFrontier {
    pub binding: BindingMaterialization,
    pub attachment: AttachmentMaterialization,
    pub summary: ConversationSummaryFrontier,
    pub last_seq: u64,
}

#[derive(Debug, Clone)]
pub struct ConversationReplay {
    pub records: Vec<ConversationEventRecordV2>,
    pub repairs: Vec<EventLogRepairWarning>,
    pub frontier: ConversationFrontier,
}

impl ConversationReplay {
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.frontier.last_seq
    }

    #[must_use]
    pub fn last_recorded_at_utc(&self) -> Option<DateTime<Utc>> {
        self.frontier.summary.last_activity_at_utc
    }
}

#[cfg(test)]
thread_local! {
    static APPLY_EVENT_COUNT: Cell<u64> = const { Cell::new(0) };
    static FULL_MATERIALIZATION_COUNT: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_operation_counters() {
    APPLY_EVENT_COUNT.set(0);
    FULL_MATERIALIZATION_COUNT.set(0);
}

#[cfg(test)]
pub(crate) fn operation_counters() -> (u64, u64) {
    (APPLY_EVENT_COUNT.get(), FULL_MATERIALIZATION_COUNT.get())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingMaterialization {
    pub current: Option<AgentSessionBinding>,
    pub history: Vec<AgentSessionBinding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentMaterialization {
    pub current: Option<ProjectAttachment>,
    pub history: Vec<ProjectAttachment>,
    pub has_events: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLogErrorKind {
    Io,
    CorruptRecord,
    UnsupportedSchema,
    ConversationMismatch,
    StreamMismatch,
    SequenceConflict,
    InvalidBindingHistory,
    InvalidAttachmentHistory,
    Durability,
}

#[derive(Debug)]
pub struct EventLogError {
    pub code: ConversationErrorCode,
    pub kind: EventLogErrorKind,
    pub conversation_id: ConversationId,
    pub path: PathBuf,
    pub detail: String,
}

impl fmt::Display for EventLogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} for Conversation {} at '{}': {}",
            self.code,
            self.conversation_id,
            self.path.display(),
            self.detail
        )
    }
}

impl std::error::Error for EventLogError {}

pub type Result<T> = std::result::Result<T, EventLogError>;

/// Load, conservatively repair, merge, and materialize every canonical stream.
pub fn replay_conversation(
    directory: &Path,
    conversation_id: ConversationId,
    durable_fs: &DurableFileSystem,
) -> Result<ConversationReplay> {
    let mut records = Vec::new();
    let mut repairs = Vec::new();
    for stream in [
        ConversationEventStream::Messages,
        ConversationEventStream::ToolCalls,
        ConversationEventStream::Bindings,
        ConversationEventStream::Attachments,
    ] {
        let loaded = load_stream(directory, conversation_id, stream, durable_fs)?;
        records.extend(loaded.records);
        repairs.extend(loaded.repairs);
    }

    records.sort_by_key(|record| record.seq);
    let mut previous = 0u64;
    for record in &records {
        if record.seq == 0 || record.seq <= previous {
            return Err(error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::SequenceConflict,
                conversation_id,
                directory,
                format!("global sequence conflict at seq {}", record.seq),
            ));
        }
        previous = record.seq;
    }

    let mut frontier = ConversationFrontier::default();
    for record in &records {
        apply_event(&mut frontier, record)?;
    }
    Ok(ConversationReplay {
        records,
        repairs,
        frontier,
    })
}

/// Validate and materialize an already seq-ordered record set.
///
/// This compatibility helper is intentionally reserved for explicit recovery/tests. Live appends
/// call [`apply_event`] once against the compact in-memory frontier.
pub fn materialize_records(
    records: &[ConversationEventRecordV2],
    conversation_id: ConversationId,
    directory: &Path,
) -> Result<(BindingMaterialization, AttachmentMaterialization)> {
    #[cfg(test)]
    FULL_MATERIALIZATION_COUNT.set(FULL_MATERIALIZATION_COUNT.get() + 1);
    let mut frontier = ConversationFrontier::default();
    for record in records {
        if record.conversation_id != conversation_id {
            return Err(error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::ConversationMismatch,
                conversation_id,
                directory,
                format!("record seq {} has a mismatched conversationId", record.seq),
            ));
        }
        apply_event(&mut frontier, record)?;
    }
    Ok((frontier.binding, frontier.attachment))
}

/// Apply one validated canonical event to the compact Conversation frontier.
pub fn apply_event(
    frontier: &mut ConversationFrontier,
    record: &ConversationEventRecordV2,
) -> Result<()> {
    let path = Path::new(record.type_.stream().file_name());
    if record.schema_version != CONVERSATION_EVENT_SCHEMA_VERSION {
        return Err(error(
            ConversationErrorCode::ConversationUnsupportedSchema,
            EventLogErrorKind::UnsupportedSchema,
            record.conversation_id,
            path,
            format!(
                "event schemaVersion {} is unsupported; expected {}",
                record.schema_version, CONVERSATION_EVENT_SCHEMA_VERSION
            ),
        ));
    }
    if record.seq == 0 || record.seq <= frontier.last_seq {
        return Err(error(
            ConversationErrorCode::ConversationRecoveryRequired,
            EventLogErrorKind::SequenceConflict,
            record.conversation_id,
            path,
            format!("global sequence conflict at seq {}", record.seq),
        ));
    }

    apply_binding_event(&mut frontier.binding, record, path)?;
    apply_attachment_event(&mut frontier.attachment, record, path)?;
    apply_summary_event(&mut frontier.summary, record)?;
    frontier.last_seq = record.seq;
    #[cfg(test)]
    APPLY_EVENT_COUNT.set(APPLY_EVENT_COUNT.get() + 1);
    Ok(())
}

struct LoadedStream {
    records: Vec<ConversationEventRecordV2>,
    repairs: Vec<EventLogRepairWarning>,
}

fn load_stream(
    directory: &Path,
    conversation_id: ConversationId,
    stream: ConversationEventStream,
    durable_fs: &DurableFileSystem,
) -> Result<LoadedStream> {
    let path = directory.join(stream.file_name());
    let bytes = fs::read(&path).map_err(|source| {
        error(
            ConversationErrorCode::ConversationRecoveryRequired,
            EventLogErrorKind::Io,
            conversation_id,
            &path,
            format!("required stream cannot be read: {source}"),
        )
    })?;
    let mut records = Vec::new();
    let mut repairs = Vec::new();
    let mut offset = 0usize;
    let mut previous_seq = 0u64;

    while offset < bytes.len() {
        let remainder = &bytes[offset..];
        let newline = remainder.iter().position(|byte| *byte == b'\n');
        let Some(position) = newline else {
            let backup_name = format!("{}.corrupt-{}.bak", stream.file_name(), Uuid::new_v4());
            let backup = directory.join(&backup_name);
            durable_fs
                .replace_bytes(&backup, &bytes)
                .map_err(|source| {
                    error(
                        ConversationErrorCode::ConversationDurabilityFailed,
                        EventLogErrorKind::Durability,
                        conversation_id,
                        &path,
                        format!("torn-tail backup failed: {source}"),
                    )
                })?;
            durable_fs
                .replace_bytes(&path, &bytes[..offset])
                .map_err(|source| {
                    error(
                        ConversationErrorCode::ConversationDurabilityFailed,
                        EventLogErrorKind::Durability,
                        conversation_id,
                        &path,
                        format!("torn-tail truncation failed: {source}"),
                    )
                })?;
            let truncated_bytes = (bytes.len() - offset) as u64;
            log::warn!(
                "[conversation-repository] torn tail repaired conversation_id={} stream={} truncated_bytes={}",
                conversation_id,
                stream.file_name(),
                truncated_bytes
            );
            repairs.push(EventLogRepairWarning {
                conversation_id,
                stream: stream.file_name().to_string(),
                backup_file: backup_name,
                truncated_bytes,
            });
            break;
        };

        let line = &remainder[..position];
        if line.is_empty() {
            return Err(error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::CorruptRecord,
                conversation_id,
                &path,
                format!("empty JSONL record at byte offset {offset}"),
            ));
        }
        let value: Value = serde_json::from_slice(line).map_err(|source| {
            error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::CorruptRecord,
                conversation_id,
                &path,
                format!("invalid newline-terminated JSON record at byte offset {offset}: {source}"),
            )
        })?;
        let found_version = value.get("schemaVersion").and_then(Value::as_u64);
        if found_version != Some(u64::from(CONVERSATION_EVENT_SCHEMA_VERSION)) {
            return Err(error(
                ConversationErrorCode::ConversationUnsupportedSchema,
                EventLogErrorKind::UnsupportedSchema,
                conversation_id,
                &path,
                format!(
                    "event schemaVersion {:?} is unsupported; expected {}",
                    found_version, CONVERSATION_EVENT_SCHEMA_VERSION
                ),
            ));
        }
        let canonical_id = conversation_id.to_string();
        if value.get("conversationId").and_then(Value::as_str) != Some(canonical_id.as_str()) {
            return Err(error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::ConversationMismatch,
                conversation_id,
                &path,
                format!("record at byte offset {offset} has a non-canonical or mismatched conversationId"),
            ));
        }
        let record: ConversationEventRecordV2 =
            serde_json::from_value(value).map_err(|source| {
                error(
                    ConversationErrorCode::ConversationRecoveryRequired,
                    EventLogErrorKind::CorruptRecord,
                    conversation_id,
                    &path,
                    format!("invalid v2 event at byte offset {offset}: {source}"),
                )
            })?;
        if record.conversation_id != conversation_id {
            return Err(error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::ConversationMismatch,
                conversation_id,
                &path,
                format!("record seq {} has a mismatched conversationId", record.seq),
            ));
        }
        if record.type_.stream() != stream {
            return Err(error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::StreamMismatch,
                conversation_id,
                &path,
                format!("record seq {} is routed to the wrong stream", record.seq),
            ));
        }
        if record.seq == 0 || record.seq <= previous_seq {
            return Err(error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::SequenceConflict,
                conversation_id,
                &path,
                format!(
                    "physical stream sequence decreases or duplicates at {}",
                    record.seq
                ),
            ));
        }
        previous_seq = record.seq;
        records.push(record);
        offset += position + 1;
    }

    Ok(LoadedStream { records, repairs })
}

fn apply_binding_event(
    materialized: &mut BindingMaterialization,
    record: &ConversationEventRecordV2,
    path: &Path,
) -> Result<()> {
    match record.type_ {
        ConversationEventType::BindingBound => {
            let payload = binding_payload(record, record.conversation_id, path)?;
            validate_binding(
                &payload.binding,
                AgentSessionBindingState::Active,
                record,
                path,
            )?;
            if materialized.current.is_some()
                || materialized
                    .history
                    .iter()
                    .any(|entry| entry.binding_id == payload.binding.binding_id)
            {
                return Err(history_error(
                    record.conversation_id,
                    path,
                    record.seq,
                    "binding_bound requires no current binding and a new bindingId",
                ));
            }
            materialized.history.push(payload.binding.clone());
            materialized.current = Some(payload.binding);
        }
        ConversationEventType::BindingDetached => {
            apply_same_binding_transition(
                materialized,
                record,
                path,
                AgentSessionBindingState::Active,
                AgentSessionBindingState::Detached,
                "binding_detached requires the current active opaque binding",
            )?;
        }
        ConversationEventType::BindingRebound => {
            apply_same_binding_transition(
                materialized,
                record,
                path,
                AgentSessionBindingState::Detached,
                AgentSessionBindingState::Active,
                "binding_rebound requires the same detached opaque binding",
            )?;
        }
        ConversationEventType::BindingSuspended => {
            apply_same_binding_transition(
                materialized,
                record,
                path,
                AgentSessionBindingState::Active,
                AgentSessionBindingState::Suspended,
                "binding_suspended requires the current active opaque binding",
            )?;
        }
        ConversationEventType::BindingReplaced => {
            let payload: BindingReplacementPayloadV1 =
                serde_json::from_value(record.payload.clone()).map_err(|source| {
                    history_error(
                        record.conversation_id,
                        path,
                        record.seq,
                        &format!("invalid binding_replaced payload: {source}"),
                    )
                })?;
            validate_binding(
                &payload.previous_binding,
                AgentSessionBindingState::Replaced,
                record,
                path,
            )?;
            validate_binding(
                &payload.binding,
                AgentSessionBindingState::Active,
                record,
                path,
            )?;
            let existing = materialized.current.as_ref().ok_or_else(|| {
                history_error(
                    record.conversation_id,
                    path,
                    record.seq,
                    "binding transition requires a current binding",
                )
            })?;
            if !same_opaque_binding(existing, &payload.previous_binding)
                || payload.binding.binding_id == payload.previous_binding.binding_id
                || materialized
                    .history
                    .iter()
                    .any(|entry| entry.binding_id == payload.binding.binding_id)
            {
                return Err(history_error(
                    record.conversation_id,
                    path,
                    record.seq,
                    "binding_replaced must retain the current old binding and add a new bindingId",
                ));
            }
            let previous_index = materialized
                .history
                .iter()
                .position(|entry| entry.binding_id == payload.previous_binding.binding_id)
                .ok_or_else(|| {
                    history_error(
                        record.conversation_id,
                        path,
                        record.seq,
                        "current binding is missing from binding history",
                    )
                })?;
            materialized.history[previous_index] = payload.previous_binding;
            materialized.history.push(payload.binding.clone());
            materialized.current = Some(payload.binding);
        }
        _ => {}
    }
    Ok(())
}

fn apply_same_binding_transition(
    materialized: &mut BindingMaterialization,
    record: &ConversationEventRecordV2,
    path: &Path,
    previous_state: AgentSessionBindingState,
    next_state: AgentSessionBindingState,
    failure_detail: &str,
) -> Result<()> {
    let payload = binding_payload(record, record.conversation_id, path)?;
    validate_binding(&payload.binding, next_state, record, path)?;
    let existing = materialized.current.as_ref().ok_or_else(|| {
        history_error(
            record.conversation_id,
            path,
            record.seq,
            "binding transition requires a current binding",
        )
    })?;
    if existing.state != previous_state || !same_opaque_binding(existing, &payload.binding) {
        return Err(history_error(
            record.conversation_id,
            path,
            record.seq,
            failure_detail,
        ));
    }
    let history_index = materialized
        .history
        .iter()
        .position(|entry| entry.binding_id == payload.binding.binding_id)
        .ok_or_else(|| {
            history_error(
                record.conversation_id,
                path,
                record.seq,
                "current binding is missing from binding history",
            )
        })?;
    materialized.history[history_index] = payload.binding.clone();
    materialized.current = Some(payload.binding);
    Ok(())
}

fn apply_attachment_event(
    materialized: &mut AttachmentMaterialization,
    record: &ConversationEventRecordV2,
    path: &Path,
) -> Result<()> {
    if !matches!(
        record.type_,
        ConversationEventType::ProjectAttached | ConversationEventType::ProjectDetached
    ) {
        return Ok(());
    }
    let payload: ProjectAttachmentEventPayloadV1 = serde_json::from_value(record.payload.clone())
        .map_err(|source| {
        attachment_history_error(
            record.conversation_id,
            path,
            record.seq,
            &format!("invalid attachment payload: {source}"),
        )
    })?;
    if payload.attachment.schema_version != PROJECT_ATTACHMENT_SCHEMA_VERSION {
        return Err(attachment_history_error(
            record.conversation_id,
            path,
            record.seq,
            "unsupported project attachment schemaVersion",
        ));
    }
    match record.type_ {
        ConversationEventType::ProjectAttached => {
            if materialized.current.is_some() {
                return Err(attachment_history_error(
                    record.conversation_id,
                    path,
                    record.seq,
                    "project_attached requires no materialized attachment",
                ));
            }
            materialized.history.push(payload.attachment.clone());
            materialized.current = Some(payload.attachment);
        }
        ConversationEventType::ProjectDetached => {
            if let Some(current) = &materialized.current {
                if current.project_id != payload.attachment.project_id {
                    return Err(attachment_history_error(
                        record.conversation_id,
                        path,
                        record.seq,
                        "project_detached does not match the current attachment",
                    ));
                }
            }
            if !materialized
                .history
                .iter()
                .any(|entry| entry == &payload.attachment)
            {
                materialized.history.push(payload.attachment);
            }
            materialized.current = None;
        }
        _ => unreachable!("attachment variants were filtered above"),
    }
    materialized.has_events = true;
    Ok(())
}

fn apply_summary_event(
    summary: &mut ConversationSummaryFrontier,
    record: &ConversationEventRecordV2,
) -> Result<()> {
    let path = Path::new(record.type_.stream().file_name());
    match record.type_.stream() {
        ConversationEventStream::Messages => {
            summary.message_count = summary.message_count.checked_add(1).ok_or_else(|| {
                error(
                    ConversationErrorCode::ConversationRecoveryRequired,
                    EventLogErrorKind::CorruptRecord,
                    record.conversation_id,
                    path,
                    format!("message count overflow at seq {}", record.seq),
                )
            })?;
        }
        ConversationEventStream::ToolCalls => {
            summary.tool_count = summary.tool_count.checked_add(1).ok_or_else(|| {
                error(
                    ConversationErrorCode::ConversationRecoveryRequired,
                    EventLogErrorKind::CorruptRecord,
                    record.conversation_id,
                    path,
                    format!("tool count overflow at seq {}", record.seq),
                )
            })?;
        }
        ConversationEventStream::Bindings | ConversationEventStream::Attachments => {}
    }
    summary.last_activity_at_utc = Some(
        summary
            .last_activity_at_utc
            .map_or(record.recorded_at_utc, |current| {
                current.max(record.recorded_at_utc)
            }),
    );

    match record.type_ {
        ConversationEventType::UserPrompt if summary.title.is_none() => {
            summary.title = Some(derive_title(&record.payload));
            summary.title_source = Some(ConversationTitleSource::DerivedFirstMessage);
        }
        ConversationEventType::SessionInfoUpdate
            if title_precedence(summary.title_source)
                < title_precedence(Some(ConversationTitleSource::BackgroundGenerated)) =>
        {
            summary.title = record
                .payload
                .get("title")
                .and_then(Value::as_str)
                .map(normalize_title);
            summary.title_source = Some(ConversationTitleSource::AgentSupplied);
        }
        ConversationEventType::LocalTitleGenerated => {
            let source = if record.payload.get("titleSource").and_then(Value::as_str)
                == Some("local_alias")
            {
                ConversationTitleSource::LocalAlias
            } else {
                ConversationTitleSource::BackgroundGenerated
            };
            if title_precedence(summary.title_source) <= title_precedence(Some(source)) {
                if let Some(title) = record
                    .payload
                    .get("title")
                    .and_then(Value::as_str)
                    .map(normalize_title)
                {
                    summary.title = Some(title);
                    summary.title_source = Some(source);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn title_precedence(source: Option<ConversationTitleSource>) -> u8 {
    match source {
        None => 0,
        Some(ConversationTitleSource::DerivedFirstMessage) => 1,
        Some(ConversationTitleSource::AgentSupplied) => 2,
        Some(ConversationTitleSource::BackgroundGenerated) => 3,
        Some(ConversationTitleSource::LocalAlias) => 4,
    }
}

fn derive_title(payload: &Value) -> String {
    let text = payload
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .unwrap_or("Untitled Chat");
    normalize_title(text)
}

fn normalize_title(text: &str) -> String {
    fn strip_wrappers(mut value: &str) -> &str {
        loop {
            let next = value
                .trim()
                .trim_matches(['"', '\'', '`'])
                .trim_matches('_')
                .trim_matches('*')
                .trim();
            if next == value {
                return next;
            }
            value = next;
        }
    }

    let mut lines = text
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let mut sanitized = strip_wrappers(lines.next().unwrap_or_default());
    let lowercase = sanitized.to_ascii_lowercase();
    const PREAMBLES: &[&str] = &[
        "sure! here's the title:",
        "sure, here's the title:",
        "here's the title:",
        "the title is:",
        "title:",
    ];
    if let Some(prefix) = PREAMBLES
        .iter()
        .find(|prefix| lowercase.starts_with(**prefix))
    {
        sanitized = strip_wrappers(&sanitized[prefix.len()..]);
        if sanitized.is_empty() {
            sanitized = strip_wrappers(lines.next().unwrap_or_default());
        }
    } else if lowercase == "what should we do?" {
        sanitized = strip_wrappers(lines.next().unwrap_or_default());
    }
    if sanitized.is_empty() {
        return "Untitled Chat".to_string();
    }
    let bounded = sanitized.chars().take(48).collect::<String>();
    if sanitized.chars().count() > 48 {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn binding_payload(
    record: &ConversationEventRecordV2,
    conversation_id: ConversationId,
    path: &Path,
) -> Result<BindingEventPayloadV1> {
    serde_json::from_value(record.payload.clone()).map_err(|source| {
        history_error(
            conversation_id,
            path,
            record.seq,
            &format!("invalid binding payload: {source}"),
        )
    })
}

fn validate_binding(
    binding: &AgentSessionBinding,
    expected_state: AgentSessionBindingState,
    record: &ConversationEventRecordV2,
    path: &Path,
) -> Result<()> {
    if binding.schema_version != AGENT_SESSION_BINDING_SCHEMA_VERSION
        || binding.state != expected_state
        || binding.agent_session_id.is_empty()
        || binding.runtime_agent_id.is_empty()
        || binding.stable_agent_namespace.is_empty()
        || binding.execution_cwd.is_empty()
    {
        return Err(history_error(
            record.conversation_id,
            path,
            record.seq,
            "binding snapshot has an invalid schema, state, or required opaque field",
        ));
    }
    Ok(())
}

fn same_opaque_binding(left: &AgentSessionBinding, right: &AgentSessionBinding) -> bool {
    left.binding_id == right.binding_id
        && left.agent_session_id == right.agent_session_id
        && left.runtime_agent_id == right.runtime_agent_id
        && left.stable_agent_namespace == right.stable_agent_namespace
        && left.execution_cwd == right.execution_cwd
        && left.bound_at_utc == right.bound_at_utc
}

fn history_error(
    conversation_id: ConversationId,
    path: &Path,
    seq: u64,
    detail: &str,
) -> EventLogError {
    error(
        ConversationErrorCode::ConversationRecoveryRequired,
        EventLogErrorKind::InvalidBindingHistory,
        conversation_id,
        path,
        format!("invalid binding history at seq {seq}: {detail}"),
    )
}

fn attachment_history_error(
    conversation_id: ConversationId,
    path: &Path,
    seq: u64,
    detail: &str,
) -> EventLogError {
    error(
        ConversationErrorCode::ConversationRecoveryRequired,
        EventLogErrorKind::InvalidAttachmentHistory,
        conversation_id,
        path,
        format!("invalid attachment history at seq {seq}: {detail}"),
    )
}

fn error(
    code: ConversationErrorCode,
    kind: EventLogErrorKind,
    conversation_id: ConversationId,
    path: &Path,
    detail: String,
) -> EventLogError {
    EventLogError {
        code,
        kind,
        conversation_id,
        path: path.to_path_buf(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::contracts::parse_created_at_utc;
    use crate::conversation::durable_fs::DirectoryPermissions;
    use serde_json::json;
    use std::io::Write;
    use tempfile::TempDir;

    const ID: &str = "018f7a1c-1b4d-7c8a-9f01-0123456789ab";

    fn fixture() -> (TempDir, PathBuf, ConversationId, DurableFileSystem) {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().canonicalize().unwrap().join(ID);
        let durable_fs = DurableFileSystem::new();
        durable_fs
            .create_dir_durable(&directory, DirectoryPermissions::PrivateOwnerOnly)
            .unwrap();
        for file in EVENT_LOG_FILES {
            durable_fs
                .replace_bytes(&directory.join(file), b"")
                .unwrap();
        }
        (
            temp,
            directory,
            ConversationId::parse(ID).unwrap(),
            durable_fs,
        )
    }

    fn record(seq: u64, type_: ConversationEventType) -> ConversationEventRecordV2 {
        ConversationEventRecordV2::new(
            ConversationId::parse(ID).unwrap(),
            seq,
            parse_created_at_utc(&format!("2026-08-15T09:45:{seq:02}.000Z")).unwrap(),
            type_,
            json!({}),
        )
    }

    fn append(
        durable_fs: &DurableFileSystem,
        directory: &Path,
        record: &ConversationEventRecordV2,
    ) {
        durable_fs
            .append_jsonl(
                &directory.join(record.type_.stream().file_name()),
                &serde_json::to_vec(record).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn incremental_frontier_preserves_distinct_events_title_precedence_and_constant_work() {
        reset_operation_counters();
        let conversation_id = ConversationId::parse(ID).unwrap();
        let recorded_at_utc = parse_created_at_utc("2026-08-15T09:45:15.000Z").unwrap();
        let mut frontier = ConversationFrontier::default();
        let fixtures = [
            (
                ConversationEventType::UserPrompt,
                json!({"content":[{"type":"text","text":"Derived title"}]}),
            ),
            (
                ConversationEventType::SessionInfoUpdate,
                json!({"title":"Agent title"}),
            ),
            (
                ConversationEventType::LocalTitleGenerated,
                json!({"title":"Background title"}),
            ),
            (
                ConversationEventType::SessionInfoUpdate,
                json!({"title":"Must not overwrite"}),
            ),
            (
                ConversationEventType::ToolCall,
                json!({"toolCall":{"id":"one"}}),
            ),
            (
                ConversationEventType::ToolCallUpdate,
                json!({"update":{"id":"one","status":"completed"}}),
            ),
            (
                ConversationEventType::LocalTitleGenerated,
                json!({"title":"Local alias","titleSource":"local_alias"}),
            ),
            (
                ConversationEventType::LocalTitleGenerated,
                json!({"title":"Must not replace alias"}),
            ),
            (
                ConversationEventType::SessionInfoUpdate,
                json!({"title":42}),
            ),
        ];
        for seq in 1..=10_000_u64 {
            let (type_, payload) = fixtures
                .get((seq - 1) as usize)
                .cloned()
                .unwrap_or((ConversationEventType::MessageChunk, json!({"role":"agent"})));
            apply_event(
                &mut frontier,
                &ConversationEventRecordV2::new(
                    conversation_id,
                    seq,
                    recorded_at_utc,
                    type_,
                    payload,
                ),
            )
            .unwrap();
        }
        assert_eq!(frontier.last_seq, 10_000);
        assert_eq!(frontier.summary.title.as_deref(), Some("Local alias"));
        assert_eq!(
            frontier.summary.title_source,
            Some(ConversationTitleSource::LocalAlias)
        );
        assert_eq!(frontier.summary.tool_count, 2);
        assert_eq!(frontier.summary.message_count, 9_998);
        assert_eq!(operation_counters(), (10_000, 0));
    }

    #[test]
    fn replay_merges_all_streams_by_one_global_sequence() {
        let (_temp, directory, id, durable_fs) = fixture();
        for record in [
            record(1, ConversationEventType::UserPrompt),
            record(3, ConversationEventType::ToolCall),
            record(2, ConversationEventType::MessageChunk),
            record(4, ConversationEventType::PromptComplete),
        ] {
            append(&durable_fs, &directory, &record);
        }
        let replay = replay_conversation(&directory, id, &durable_fs).unwrap();
        assert_eq!(
            replay
                .records
                .iter()
                .map(|record| record.seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(replay.last_seq(), 4);
    }

    #[test]
    fn torn_final_record_is_backed_up_and_truncated_to_complete_newline() {
        let (_temp, directory, id, durable_fs) = fixture();
        let complete = record(1, ConversationEventType::MessageChunk);
        append(&durable_fs, &directory, &complete);
        let path = directory.join(MESSAGES_FILE);
        let complete_bytes = fs::read(&path).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(br#"{"schemaVersion":2,"seq":2"#)
            .unwrap();

        let replay = replay_conversation(&directory, id, &durable_fs).unwrap();
        assert_eq!(replay.last_seq(), 1);
        assert_eq!(replay.repairs.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), complete_bytes);
        let backup = directory.join(&replay.repairs[0].backup_file);
        assert!(backup.exists());
        assert!(fs::read(backup).unwrap().len() > complete_bytes.len());
    }

    #[test]
    fn newline_terminated_or_middle_corruption_is_not_rewritten() {
        let (_temp, directory, id, durable_fs) = fixture();
        let path = directory.join(MESSAGES_FILE);
        let bytes = b"{bad}\n{also-bad}\n";
        fs::write(&path, bytes).unwrap();
        let error = replay_conversation(&directory, id, &durable_fs).unwrap_err();
        assert_eq!(
            error.code,
            ConversationErrorCode::ConversationRecoveryRequired
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-")));
    }

    #[test]
    fn duplicate_global_sequence_wrong_stream_and_future_schema_fail_closed() {
        let (_temp, directory, id, durable_fs) = fixture();
        append(
            &durable_fs,
            &directory,
            &record(1, ConversationEventType::MessageChunk),
        );
        append(
            &durable_fs,
            &directory,
            &record(1, ConversationEventType::ToolCall),
        );
        let duplicate = replay_conversation(&directory, id, &durable_fs).unwrap_err();
        assert_eq!(duplicate.kind, EventLogErrorKind::SequenceConflict);

        for file in EVENT_LOG_FILES {
            durable_fs
                .replace_bytes(&directory.join(file), b"")
                .unwrap();
        }
        let wrong_stream = record(2, ConversationEventType::ToolCall);
        durable_fs
            .append_jsonl(
                &directory.join(MESSAGES_FILE),
                &serde_json::to_vec(&wrong_stream).unwrap(),
            )
            .unwrap();
        assert_eq!(
            replay_conversation(&directory, id, &durable_fs)
                .unwrap_err()
                .kind,
            EventLogErrorKind::StreamMismatch
        );

        for file in EVENT_LOG_FILES {
            durable_fs
                .replace_bytes(&directory.join(file), b"")
                .unwrap();
        }
        let mut future =
            serde_json::to_value(record(1, ConversationEventType::UserPrompt)).unwrap();
        future["schemaVersion"] = json!(99);
        durable_fs
            .append_jsonl(
                &directory.join(MESSAGES_FILE),
                &serde_json::to_vec(&future).unwrap(),
            )
            .unwrap();
        let future = replay_conversation(&directory, id, &durable_fs).unwrap_err();
        assert_eq!(
            future.code,
            ConversationErrorCode::ConversationUnsupportedSchema
        );

        for file in EVENT_LOG_FILES {
            durable_fs
                .replace_bytes(&directory.join(file), b"")
                .unwrap();
        }
        let mut uppercase =
            serde_json::to_value(record(1, ConversationEventType::UserPrompt)).unwrap();
        uppercase["conversationId"] = json!(ID.to_ascii_uppercase());
        durable_fs
            .append_jsonl(
                &directory.join(MESSAGES_FILE),
                &serde_json::to_vec(&uppercase).unwrap(),
            )
            .unwrap();
        assert_eq!(
            replay_conversation(&directory, id, &durable_fs)
                .unwrap_err()
                .kind,
            EventLogErrorKind::ConversationMismatch
        );
    }
}
