//! Host-owned Conversation domain contracts and services.

pub mod contracts;

pub use contracts::{
    format_created_at_utc, parse_created_at_utc, AgentSessionBinding, AgentSessionBindingState,
    ConversationCreator, ConversationErrorCode, ConversationId, ConversationIdPathError,
    ConversationLifecycleState, ConversationRecordV2, CreatedAtUtcError, CreationPartition,
    ExecutionTarget, ProjectAttachment, TerminalResourceRef, AGENT_SESSION_BINDING_SCHEMA_VERSION,
    CONVERSATION_SCHEMA_VERSION, PROJECT_ATTACHMENT_SCHEMA_VERSION,
    TERMINAL_RESOURCE_REF_SCHEMA_VERSION,
};
