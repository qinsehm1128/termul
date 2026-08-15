import type { ConversationId } from '@shared/types/conversation.types'
import type {
  RecoveryActionResult,
  ResolveRecoveryItemRequest
} from '@shared/types/conversation-recovery.types'
import { parseResolveRecoveryItemRequest } from '@shared/types/conversation-recovery.types'
import type { IpcResult } from '@shared/types/ipc.types'
import type {
  SessionWorkspaceApi,
  SessionWorkspaceLoadOutcome,
  SessionWorkspaceV1,
  SessionWorkspaceWriteOutcome
} from '@shared/types/session-workspace.types'
import { type InvokeArgs, invoke } from '@tauri-apps/api/core'
import { isTauriContext } from './tauri-runtime'

const canonicalUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

async function invokeIpc<T>(command: string, args?: InvokeArgs): Promise<IpcResult<T>> {
  try {
    return await invoke<IpcResult<T>>(command, args)
  } catch (error) {
    return {
      success: false,
      error: error instanceof Error ? error.message : String(error),
      code: 'INVOKE_ERROR'
    }
  }
}

function invalidConversationId(): IpcResult<never> {
  return {
    success: false,
    error: 'conversationId must be a canonical lowercase-hyphenated UUID',
    code: 'CONVERSATION_INVALID_ID'
  }
}

export function createTauriSessionWorkspaceApi(): SessionWorkspaceApi {
  return {
    async getWorkspace(
      conversationId: ConversationId
    ): Promise<IpcResult<SessionWorkspaceLoadOutcome>> {
      if (!canonicalUuid.test(conversationId)) return invalidConversationId()
      if (!isTauriContext()) {
        return {
          success: false,
          error: 'session_workspace_get requires the Tauri runtime',
          code: 'INVOKE_ERROR'
        }
      }
      return invokeIpc('session_workspace_get', { conversationId })
    },

    async writeWorkspace(
      conversationId: ConversationId,
      basedRevision: number | null,
      workspace: SessionWorkspaceV1
    ): Promise<IpcResult<SessionWorkspaceWriteOutcome>> {
      if (!canonicalUuid.test(conversationId) || workspace.conversationId !== conversationId) {
        return invalidConversationId()
      }
      if (!isTauriContext()) {
        return {
          success: false,
          error: 'session_workspace_write requires the Tauri runtime',
          code: 'INVOKE_ERROR'
        }
      }
      return invokeIpc('session_workspace_write', { conversationId, basedRevision, workspace })
    },

    async resolveRecovery(
      request: ResolveRecoveryItemRequest
    ): Promise<IpcResult<RecoveryActionResult>> {
      let parsed: ResolveRecoveryItemRequest
      try {
        parsed = parseResolveRecoveryItemRequest(request)
      } catch (error) {
        return {
          success: false,
          error: error instanceof Error ? error.message : String(error),
          code: 'VALIDATION_ERROR'
        }
      }
      if (!isTauriContext()) {
        return {
          success: false,
          error: 'conversation_recovery_resolve requires the Tauri runtime',
          code: 'INVOKE_ERROR'
        }
      }
      return invokeIpc('conversation_recovery_resolve', { request: parsed })
    }
  }
}
