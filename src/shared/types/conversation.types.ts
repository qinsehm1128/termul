/**
 * Runtime-neutral Conversation identity, lifecycle, and resource-reference contracts.
 *
 * ConversationId is allocated by Termul before ACP session creation. ACP session ids remain
 * opaque replaceable bindings and must never be used as Conversation identity or path keys.
 */

export const CONVERSATION_SCHEMA_VERSION = 2 as const
export const PROJECT_ATTACHMENT_SCHEMA_VERSION = 1 as const
export const AGENT_SESSION_BINDING_SCHEMA_VERSION = 1 as const
export const TERMINAL_RESOURCE_REF_SCHEMA_VERSION = 1 as const

/** Canonical lowercase-hyphenated Termul-owned UUID. */
export type ConversationId = string

/** UTC date partition derived only from immutable createdAtUtc. */
export interface CreationPartition {
  year: number
  month: number
  day: number
  path: string
}

/** Explicit execution choice; it never changes the independent workspaceCwd. */
export type ExecutionTarget =
  | { kind: 'workspace' }
  | { kind: 'project_root'; projectId: string; projectRoot: string }
  | {
      kind: 'worktree'
      projectId: string
      worktreePath: string
      worktreeBranch: string
    }

/** Optional project attribution/context. Attaching it never mutates workspaceCwd. */
export interface ProjectAttachment {
  schemaVersion: typeof PROJECT_ATTACHMENT_SCHEMA_VERSION
  projectId: string
  attachedAtUtc: string
  projectPathSnapshot: string
  worktreePath: string | null
  worktreeBranch: string | null
}

export const AGENT_SESSION_BINDING_STATES = ['active', 'detached', 'suspended', 'replaced'] as const

export type AgentSessionBindingState = (typeof AGENT_SESSION_BINDING_STATES)[number]

/**
 * Replaceable binding to one external ACP session.
 *
 * agentSessionId is deliberately opaque and may contain non-UUID characters. executionCwd is
 * the resolved explicit target and is independent from the Conversation workspaceCwd.
 */
export interface AgentSessionBinding {
  schemaVersion: typeof AGENT_SESSION_BINDING_SCHEMA_VERSION
  bindingId: string
  agentSessionId: string
  runtimeAgentId: string
  stableAgentNamespace: string
  executionCwd: string
  boundAtUtc: string
  state: AgentSessionBindingState
}

/**
 * Non-owning reference to a PtyManager-owned terminal resource.
 *
 * Raw terminal claims, environment values, credentials, and terminal output are never persisted.
 */
export interface TerminalResourceRef {
  schemaVersion: typeof TERMINAL_RESOURCE_REF_SCHEMA_VERSION
  terminalId: string
  projectId?: string | null
}

export const CONVERSATION_LIFECYCLE_STATES = [
  'allocating_workspace',
  'initializing_agent',
  'ready',
  'agent_failed',
  'recovery_required',
  'deleted'
] as const

export type ConversationLifecycleState = (typeof CONVERSATION_LIFECYCLE_STATES)[number]

export const CONVERSATION_ERROR_CODES = [
  'CONVERSATION_INVALID_ID',
  'CONVERSATION_INVALID_CREATED_AT',
  'CONVERSATION_UNSUPPORTED_SCHEMA',
  'CONVERSATION_NOT_FOUND',
  'CONVERSATION_CORRUPT',
  'CONVERSATION_PATH_ESCAPE',
  'CONVERSATION_SYMLINK_COMPONENT',
  'CONVERSATION_DURABILITY_FAILED',
  'CONVERSATION_CREATE_FAILED',
  'CONVERSATION_BIND_FAILED',
  'CONVERSATION_RECOVERY_REQUIRED',
  'CONVERSATION_DURABILITY_UNSUPPORTED'
] as const

export type ConversationErrorCode = (typeof CONVERSATION_ERROR_CODES)[number]

/** Canonical Conversation metadata record. Binding and resource history are stored separately. */
export interface ConversationRecordV2 {
  schemaVersion: typeof CONVERSATION_SCHEMA_VERSION
  conversationId: ConversationId
  createdAtUtc: string
  creationPartition: CreationPartition
  workspaceCwd: string
  executionTarget: ExecutionTarget
  projectAttachment: ProjectAttachment | null
  lifecycleState: ConversationLifecycleState
  lastSeq: number
  createdBy: 'termul'
}
