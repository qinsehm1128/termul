//! Host-owned Conversation domain contracts and services.

pub mod contracts;
pub mod durable_fs;
pub mod locator;

pub use contracts::{
    format_created_at_utc, parse_created_at_utc, AgentSessionBinding, AgentSessionBindingState,
    ConversationCreator, ConversationErrorCode, ConversationId, ConversationIdPathError,
    ConversationLifecycleState, ConversationRecordV2, CreatedAtUtcError, CreationPartition,
    ExecutionTarget, ProjectAttachment, TerminalResourceRef, AGENT_SESSION_BINDING_SCHEMA_VERSION,
    CONVERSATION_SCHEMA_VERSION, PROJECT_ATTACHMENT_SCHEMA_VERSION,
    TERMINAL_RESOURCE_REF_SCHEMA_VERSION,
};
pub use durable_fs::{
    append_jsonl, create_dir_durable, replace_bytes, sync_file_and_namespace, CrashInjector,
    CrashPoint, DirectoryPermissions, DurabilityLevel, DurableFileSystem, DurableFsError,
    DurableWriteOutcome, NamespaceState, OwnedTempDisposition,
};
pub use locator::{
    bounded_scan, BoundedScan, ConversationLocator, LocatedConversation, LocatorError,
    SessionWorkspaceLocator, MAX_CONVERSATIONS_PER_SCAN, MAX_DIRECTORY_ENTRIES_PER_LEVEL,
};
