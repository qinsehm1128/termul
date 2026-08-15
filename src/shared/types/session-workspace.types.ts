import type { ConversationId } from './conversation.types'
import type {
  RecoveryActionResult,
  RecoveryItemV1,
  ResolveRecoveryItemRequest
} from './conversation-recovery.types'
import type { IpcResult } from './ipc.types'

export const SESSION_WORKSPACE_SCHEMA_VERSION = 1 as const

export type SessionWorkspacePaneDirection = 'horizontal' | 'vertical'

export interface SessionWorkspaceSplitNode {
  type: 'split'
  id: string
  direction: SessionWorkspacePaneDirection
  children: SessionWorkspacePaneNode[]
  sizes: number[]
}

export interface SessionWorkspaceLeafNode {
  type: 'leaf'
  id: string
  terminalIds: string[]
  editorIds: string[]
  activeTabId?: string | null
}

export type SessionWorkspacePaneNode = SessionWorkspaceSplitNode | SessionWorkspaceLeafNode

/** Passive reference to a PtyManager-owned terminal. It conveys no ownership or credential. */
export interface TerminalResourceDescriptor {
  kind: 'terminal'
  /** PtyManager-owned live resource id. */
  terminalId: string
  /** Optional renderer record id used to rebuild visible topology. */
  terminalRecordId?: string
  conversationId: ConversationId
}

/** Passive editor reference. Unsaved contents, cursor state, and viewport state are excluded. */
export interface EditorResourceDescriptor {
  kind: 'editor'
  editorId: string
  filePath: string
}

export type SessionWorkspaceResourceDescriptor =
  | TerminalResourceDescriptor
  | EditorResourceDescriptor

export type SessionWorkspaceProjectionState =
  | { status: 'native' }
  | {
      status: 'projected'
      sourcePath: string
      sourceSha256: string
      projectedResourceCount: number
      unresolvedResourceCount: number
    }
  | {
      status: 'recoveryRequired'
      recoveryIds: string[]
    }

export interface SessionWorkspaceV1 {
  schemaVersion: typeof SESSION_WORKSPACE_SCHEMA_VERSION
  conversationId: ConversationId
  revision: number
  updatedAtUtc: string
  updateIdentity?: string | null
  topology?: SessionWorkspacePaneNode | null
  activePaneId?: string | null
  resources: SessionWorkspaceResourceDescriptor[]
  projectionState: SessionWorkspaceProjectionState
}

export type SessionWorkspaceLoadOutcome =
  | { status: 'missing'; conversationId: ConversationId }
  | { status: 'loaded'; workspace: SessionWorkspaceV1 }
  | {
      status: 'recoveryRequired'
      conversationId: ConversationId
      recoveryItems: RecoveryItemV1[]
    }

export type SessionWorkspaceWriteOutcome =
  | { status: 'updated'; revision: number; updatedAtUtc: string }
  | {
      status: 'conflict'
      currentRevision: number
      currentUpdatedAtUtc: string
      currentUpdateIdentity?: string | null
    }
  | {
      status: 'recoveryRequired'
      recoveryItems: RecoveryItemV1[]
    }

export interface SessionWorkspaceWriteRequestBody {
  basedRevision: number | null
  workspace: SessionWorkspaceV1
}

export interface SessionWorkspaceApi {
  getWorkspace(conversationId: ConversationId): Promise<IpcResult<SessionWorkspaceLoadOutcome>>
  writeWorkspace(
    conversationId: ConversationId,
    basedRevision: number | null,
    workspace: SessionWorkspaceV1
  ): Promise<IpcResult<SessionWorkspaceWriteOutcome>>
  resolveRecovery(request: ResolveRecoveryItemRequest): Promise<IpcResult<RecoveryActionResult>>
}
