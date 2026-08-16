import type {
  AgentSessionBinding,
  ConversationId,
  ConversationLifecycleState,
  ExecutionTarget,
  ProjectAttachment
} from './conversation.types'

export const CONVERSATION_LIFECYCLE_ACTIONS = [
  'detachBinding',
  'rebindDetachedBinding',
  'suspendBinding',
  'replaceBinding',
  'deleteConversation'
] as const

export type ConversationLifecycleAction = (typeof CONVERSATION_LIFECYCLE_ACTIONS)[number]

export const CONVERSATION_LIFECYCLE_ERROR_CODES = [
  'CONVERSATION_CONFLICT',
  'CONVERSATION_NOT_FOUND',
  'CONVERSATION_BINDING_NOT_FOUND',
  'CONVERSATION_BINDING_NOT_ACTIVE',
  'CONVERSATION_BINDING_NOT_DETACHED',
  'CONVERSATION_BINDING_NOT_ADDRESSABLE',
  'CONVERSATION_LIVE_RESOURCES',
  'CONVERSATION_RECOVERY_REQUIRED',
  'CONVERSATION_DURABILITY_FAILED',
  'ACP_CLOSE_UNSUPPORTED',
  'ACP_CLOSE_FAILED',
  'ACP_REPLACE_FAILED',
  'ACP_COMPENSATION_FAILED',
  'VALIDATION_ERROR',
  'FORBIDDEN',
  'NETWORK_ERROR'
] as const

export type ConversationLifecycleErrorCode = (typeof CONVERSATION_LIFECYCLE_ERROR_CODES)[number]

/** Secret-safe compound detail returned with ACP_COMPENSATION_FAILED. */
export interface AcpCompensationFailure {
  conversationId: ConversationId
  primaryCode: string
  providerCloseCode?: string
  failureRecordCode?: string
  recoveryMarkerCode?: string
  recoveryRecordCode?: string
  recoveryId?: string
}

export interface ConversationLifecycleMutationRequest {
  expectedRevision: number
}

export interface ConversationReplacementRequest {
  schemaVersion: 1
  conversationId: ConversationId
  projectAttachment?: ProjectAttachment | null
  executionTarget: ExecutionTarget
}

export type ConversationDeleteBlocker =
  | {
      kind: 'liveBinding'
      count: number
      ids: string[]
    }
  | {
      kind: 'terminalResources'
      count: number
      ids: string[]
    }

export interface ConversationLifecycleUpdatedOutcome {
  status: 'updated'
  action: ConversationLifecycleAction
  conversationId: ConversationId
  previousRevision: number
  revision: number
  workspaceCwd: string
  lifecycleState: ConversationLifecycleState
  currentBinding: AgentSessionBinding | null
  previousAgentSessionId?: string | null
}

export interface ConversationLifecycleBlockedOutcome {
  status: 'blocked'
  action: 'deleteConversation'
  conversationId: ConversationId
  revision: number
  code: 'CONVERSATION_LIVE_RESOURCES'
  blockers: ConversationDeleteBlocker[]
}

export type ConversationLifecycleOutcome =
  | ConversationLifecycleUpdatedOutcome
  | ConversationLifecycleBlockedOutcome

export interface ConversationLifecycleApi {
  detachBinding(
    conversationId: ConversationId,
    expectedRevision: number
  ): Promise<ConversationLifecycleOutcome>
  rebindDetachedBinding(
    conversationId: ConversationId,
    expectedRevision: number
  ): Promise<ConversationLifecycleOutcome>
  suspendBinding(
    conversationId: ConversationId,
    expectedRevision: number
  ): Promise<ConversationLifecycleOutcome>
  replaceBinding(
    conversationId: ConversationId,
    request: ConversationReplacementRequest,
    expectedRevision: number
  ): Promise<ConversationLifecycleOutcome>
  deleteConversation(
    conversationId: ConversationId,
    expectedRevision: number
  ): Promise<ConversationLifecycleOutcome>
  subscribe(listener: (outcome: ConversationLifecycleOutcome) => void): () => void
}
