import type {
  ConversationAggregateMutationOutcome,
  ConversationId,
  ConversationRecordV2,
  ExecutionTarget,
  ProjectAttachment
} from './conversation.types'
import type { RecoveryItemV1 } from './conversation-recovery.types'
import type { IpcResult } from './ipc.types'
import type { SessionWorkspaceLoadOutcome } from './session-workspace.types'

export type {
  RecoveryAction,
  RecoveryActionResult,
  ResolveRecoveryItemRequest
} from './conversation-recovery.types'

export const LEGACY_CONVERSATION_SOURCE_KINDS = [
  'legacyStorageKey',
  'legacyAgentSessionId',
  'legacyChatHistoryId'
] as const

export type LegacyConversationSourceKind = (typeof LEGACY_CONVERSATION_SOURCE_KINDS)[number]

export interface LegacyConversationKey {
  sourceKind: LegacyConversationSourceKind
  value: string
}

export interface LegacyConversationResolution {
  conversationId: ConversationId
  canonicalRoute: `#/c/${string}`
}

export type ConversationHostKind = 'desktop' | 'standalone'
export type ConversationHostState = 'ready' | 'migrating' | 'hybrid' | 'recovery' | 'error'

export type ConversationMigrationPhase =
  | 'detected'
  | 'quiescing'
  | 'inventoried'
  | 'staging'
  | 'verifying'
  | 'cutoverPending'
  | 'committed'
  | 'observationWindow'
  | 'rollbackPending'
  | 'rolledBack'
  | 'finalized'

export type ConversationReaderPrecedence =
  | 'legacyOnly'
  | 'conversationV2First'
  | 'hybridLegacyFirst'
  | 'conversationV2Only'

export interface ConversationHostStatus {
  hostKind: ConversationHostKind
  state: ConversationHostState
  code: string
  migrationPhase: ConversationMigrationPhase
  readerPrecedence: ConversationReaderPrecedence
  recoveryItemCount: number
  recoveryItems: RecoveryItemV1[]
}

export interface ConversationOpenOutcome {
  conversation: ConversationRecordV2
  workspace: SessionWorkspaceLoadOutcome
}

export type ConversationApplicationRequestType =
  | 'conversation_host_status'
  | 'list_conversations'
  | 'get_conversation'
  | 'open_conversation'
  | 'resolve_legacy_conversation_id'
  | 'get_session_workspace'
  | 'write_session_workspace'
  | 'resolve_recovery_item'
  | 'attach_project'
  | 'detach_project'
  | 'update_execution_target'

export interface ConversationApi {
  getHostStatus(): Promise<IpcResult<ConversationHostStatus>>
  listConversations(): Promise<IpcResult<ConversationRecordV2[]>>
  getConversation(conversationId: ConversationId): Promise<IpcResult<ConversationRecordV2>>
  openConversation(conversationId: ConversationId): Promise<IpcResult<ConversationOpenOutcome>>
  resolveLegacyConversationId(
    key: LegacyConversationKey
  ): Promise<IpcResult<LegacyConversationResolution>>
  attachProject(
    conversationId: ConversationId,
    expectedRevision: number,
    attachment: ProjectAttachment
  ): Promise<IpcResult<ConversationAggregateMutationOutcome>>
  detachProject(
    conversationId: ConversationId,
    expectedRevision: number
  ): Promise<IpcResult<ConversationAggregateMutationOutcome>>
  updateExecutionTarget(
    conversationId: ConversationId,
    expectedRevision: number,
    executionTarget: ExecutionTarget
  ): Promise<IpcResult<ConversationAggregateMutationOutcome>>
  subscribeHostStatus(listener: () => void): () => void
}
