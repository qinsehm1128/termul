//! Canonical v2 Conversation JSONL routing, validation, replay, and materialization.
//!
//! Every Conversation owns one global monotonically increasing sequence across four physical
//! streams. Stream files are append-only; replay validates their physical order, merges them by
//! `seq`, and rejects duplicate or mismatched records. Recovery repairs only an unterminated final
//! line, preserving the original bytes beside the log before atomically truncating it.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

use crate::conversation::contracts::{
    format_created_at_utc, parse_created_at_utc, AgentSessionBinding, AgentSessionBindingState,
    ConversationErrorCode, ConversationId, ProjectAttachment, AGENT_SESSION_BINDING_SCHEMA_VERSION,
    PROJECT_ATTACHMENT_SCHEMA_VERSION,
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
    PromptComplete,
    ToolCall,
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
            Self::ToolCall => ConversationEventStream::ToolCalls,
            Self::BindingBound
            | Self::BindingDetached
            | Self::BindingRebound
            | Self::BindingSuspended
            | Self::BindingReplaced => ConversationEventStream::Bindings,
            Self::ProjectAttached | Self::ProjectDetached => ConversationEventStream::Attachments,
            Self::UserPrompt | Self::MessageChunk | Self::PromptComplete | Self::CreationFailed => {
                ConversationEventStream::Messages
            }
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

#[derive(Debug, Clone)]
pub struct ConversationReplay {
    pub records: Vec<ConversationEventRecordV2>,
    pub repairs: Vec<EventLogRepairWarning>,
    pub binding: BindingMaterialization,
    pub attachment: AttachmentMaterialization,
}

impl ConversationReplay {
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.records.last().map_or(0, |record| record.seq)
    }

    #[must_use]
    pub fn last_recorded_at_utc(&self) -> Option<DateTime<Utc>> {
        self.records
            .iter()
            .map(|record| record.recorded_at_utc)
            .max()
    }
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

    let (binding, attachment) = materialize_records(&records, conversation_id, directory)?;
    Ok(ConversationReplay {
        records,
        repairs,
        binding,
        attachment,
    })
}

/// Validate and materialize an already seq-ordered prospective record set before append.
pub fn materialize_records(
    records: &[ConversationEventRecordV2],
    conversation_id: ConversationId,
    directory: &Path,
) -> Result<(BindingMaterialization, AttachmentMaterialization)> {
    let mut previous = 0u64;
    for record in records {
        if record.conversation_id != conversation_id || record.seq == 0 || record.seq <= previous {
            return Err(error(
                ConversationErrorCode::ConversationRecoveryRequired,
                EventLogErrorKind::SequenceConflict,
                conversation_id,
                directory,
                format!("prospective global sequence conflict at seq {}", record.seq),
            ));
        }
        previous = record.seq;
    }
    let binding = materialize_bindings(records, conversation_id, directory)?;
    let attachment = materialize_attachments(records, conversation_id, directory)?;
    Ok((binding, attachment))
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

fn materialize_bindings(
    records: &[ConversationEventRecordV2],
    conversation_id: ConversationId,
    directory: &Path,
) -> Result<BindingMaterialization> {
    let path = directory.join(BINDINGS_FILE);
    let mut bindings: HashMap<Uuid, AgentSessionBinding> = HashMap::new();
    let mut order = Vec::new();
    let mut current: Option<Uuid> = None;

    for record in records
        .iter()
        .filter(|record| record.type_.stream() == ConversationEventStream::Bindings)
    {
        match record.type_ {
            ConversationEventType::BindingBound => {
                let payload = binding_payload(record, conversation_id, &path)?;
                validate_binding(
                    &payload.binding,
                    AgentSessionBindingState::Active,
                    record,
                    &path,
                )?;
                if current.is_some() || bindings.contains_key(&payload.binding.binding_id) {
                    return Err(history_error(
                        conversation_id,
                        &path,
                        record.seq,
                        "binding_bound requires no current binding and a new bindingId",
                    ));
                }
                current = Some(payload.binding.binding_id);
                order.push(payload.binding.binding_id);
                bindings.insert(payload.binding.binding_id, payload.binding);
            }
            ConversationEventType::BindingDetached => {
                let payload = binding_payload(record, conversation_id, &path)?;
                validate_binding(
                    &payload.binding,
                    AgentSessionBindingState::Detached,
                    record,
                    &path,
                )?;
                let existing = current_binding(&bindings, current, record, conversation_id, &path)?;
                if existing.state != AgentSessionBindingState::Active
                    || !same_opaque_binding(existing, &payload.binding)
                {
                    return Err(history_error(
                        conversation_id,
                        &path,
                        record.seq,
                        "binding_detached requires the current active opaque binding",
                    ));
                }
                bindings.insert(payload.binding.binding_id, payload.binding);
            }
            ConversationEventType::BindingRebound => {
                let payload = binding_payload(record, conversation_id, &path)?;
                validate_binding(
                    &payload.binding,
                    AgentSessionBindingState::Active,
                    record,
                    &path,
                )?;
                let existing = current_binding(&bindings, current, record, conversation_id, &path)?;
                if existing.state != AgentSessionBindingState::Detached
                    || !same_opaque_binding(existing, &payload.binding)
                {
                    return Err(history_error(
                        conversation_id,
                        &path,
                        record.seq,
                        "binding_rebound requires the same detached opaque binding",
                    ));
                }
                bindings.insert(payload.binding.binding_id, payload.binding);
            }
            ConversationEventType::BindingSuspended => {
                let payload = binding_payload(record, conversation_id, &path)?;
                validate_binding(
                    &payload.binding,
                    AgentSessionBindingState::Suspended,
                    record,
                    &path,
                )?;
                let existing = current_binding(&bindings, current, record, conversation_id, &path)?;
                if existing.state != AgentSessionBindingState::Active
                    || !same_opaque_binding(existing, &payload.binding)
                {
                    return Err(history_error(
                        conversation_id,
                        &path,
                        record.seq,
                        "binding_suspended requires the current active opaque binding",
                    ));
                }
                bindings.insert(payload.binding.binding_id, payload.binding);
            }
            ConversationEventType::BindingReplaced => {
                let payload: BindingReplacementPayloadV1 =
                    serde_json::from_value(record.payload.clone()).map_err(|source| {
                        history_error(
                            conversation_id,
                            &path,
                            record.seq,
                            &format!("invalid binding_replaced payload: {source}"),
                        )
                    })?;
                validate_binding(
                    &payload.previous_binding,
                    AgentSessionBindingState::Replaced,
                    record,
                    &path,
                )?;
                validate_binding(
                    &payload.binding,
                    AgentSessionBindingState::Active,
                    record,
                    &path,
                )?;
                let existing = current_binding(&bindings, current, record, conversation_id, &path)?;
                if !same_opaque_binding(existing, &payload.previous_binding)
                    || payload.binding.binding_id == payload.previous_binding.binding_id
                    || bindings.contains_key(&payload.binding.binding_id)
                {
                    return Err(history_error(
                        conversation_id,
                        &path,
                        record.seq,
                        "binding_replaced must retain the current old binding and add a new bindingId",
                    ));
                }
                bindings.insert(
                    payload.previous_binding.binding_id,
                    payload.previous_binding,
                );
                current = Some(payload.binding.binding_id);
                order.push(payload.binding.binding_id);
                bindings.insert(payload.binding.binding_id, payload.binding);
            }
            _ => {}
        }
    }

    let history = order
        .into_iter()
        .filter_map(|binding_id| bindings.get(&binding_id).cloned())
        .collect::<Vec<_>>();
    let current = current.and_then(|binding_id| bindings.get(&binding_id).cloned());
    Ok(BindingMaterialization { current, history })
}

fn materialize_attachments(
    records: &[ConversationEventRecordV2],
    conversation_id: ConversationId,
    directory: &Path,
) -> Result<AttachmentMaterialization> {
    let path = directory.join(ATTACHMENTS_FILE);
    let mut materialized = AttachmentMaterialization::default();
    for record in records
        .iter()
        .filter(|record| record.type_.stream() == ConversationEventStream::Attachments)
    {
        let payload: ProjectAttachmentEventPayloadV1 =
            serde_json::from_value(record.payload.clone()).map_err(|source| {
                attachment_history_error(
                    conversation_id,
                    &path,
                    record.seq,
                    &format!("invalid attachment payload: {source}"),
                )
            })?;
        if payload.attachment.schema_version != PROJECT_ATTACHMENT_SCHEMA_VERSION {
            return Err(attachment_history_error(
                conversation_id,
                &path,
                record.seq,
                "unsupported project attachment schemaVersion",
            ));
        }
        materialized.has_events = true;
        match record.type_ {
            ConversationEventType::ProjectAttached => {
                if materialized.current.is_some() {
                    return Err(attachment_history_error(
                        conversation_id,
                        &path,
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
                            conversation_id,
                            &path,
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
            _ => {}
        }
    }
    Ok(materialized)
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

fn current_binding<'a>(
    bindings: &'a HashMap<Uuid, AgentSessionBinding>,
    current: Option<Uuid>,
    record: &ConversationEventRecordV2,
    conversation_id: ConversationId,
    path: &Path,
) -> Result<&'a AgentSessionBinding> {
    current
        .and_then(|binding_id| bindings.get(&binding_id))
        .ok_or_else(|| {
            history_error(
                conversation_id,
                path,
                record.seq,
                "binding transition requires a current binding",
            )
        })
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
