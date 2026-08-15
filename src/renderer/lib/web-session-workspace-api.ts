import type { ConversationId } from '@shared/types/conversation.types'
import {
  parseResolveRecoveryItemRequest,
  type RecoveryActionResult,
  type ResolveRecoveryItemRequest
} from '@shared/types/conversation-recovery.types'
import type { IpcResult } from '@shared/types/ipc.types'
import type {
  SessionWorkspaceApi,
  SessionWorkspaceLoadOutcome,
  SessionWorkspaceV1,
  SessionWorkspaceWriteOutcome,
  SessionWorkspaceWriteRequestBody
} from '@shared/types/session-workspace.types'
import { isTauriContext } from './tauri-runtime'

const canonicalUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

type IpcBody<T> = { success: true; data?: T } | { success: false; error: string; code: string }

function serverBase(): string {
  if (isTauriContext()) return ''
  if (typeof window === 'undefined' || !window.location) return ''
  return window.location.origin
}

function networkError(detail: string): IpcResult<never> {
  return { success: false, error: detail, code: 'NETWORK_ERROR' }
}

function invalidConversationId(): IpcResult<never> {
  return {
    success: false,
    error: 'conversationId must be a canonical lowercase-hyphenated UUID',
    code: 'CONVERSATION_INVALID_ID'
  }
}

async function parseBody<T>(response: Response): Promise<IpcResult<T>> {
  if (!response.ok) return networkError(`HTTP ${response.status} ${response.statusText}`)
  try {
    const body = (await response.json()) as IpcBody<T>
    return body.success
      ? { success: true, data: body.data as T }
      : { success: false, error: body.error, code: body.code }
  } catch (error) {
    return networkError(error instanceof Error ? error.message : 'invalid JSON')
  }
}

async function getJson<T>(path: string): Promise<IpcResult<T>> {
  try {
    return await parseBody<T>(await fetch(`${serverBase()}${path}`, { method: 'GET' }))
  } catch (error) {
    return networkError(error instanceof Error ? error.message : String(error))
  }
}

async function postJson<T>(path: string, body: unknown): Promise<IpcResult<T>> {
  try {
    return await parseBody<T>(
      await fetch(`${serverBase()}${path}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body)
      })
    )
  } catch (error) {
    return networkError(error instanceof Error ? error.message : String(error))
  }
}

export const webSessionWorkspaceApi: SessionWorkspaceApi = {
  getWorkspace(conversationId: ConversationId): Promise<IpcResult<SessionWorkspaceLoadOutcome>> {
    if (!canonicalUuid.test(conversationId)) return Promise.resolve(invalidConversationId())
    return getJson(`/conversations/${encodeURIComponent(conversationId)}/workspace`)
  },

  writeWorkspace(
    conversationId: ConversationId,
    basedRevision: number | null,
    workspace: SessionWorkspaceV1
  ): Promise<IpcResult<SessionWorkspaceWriteOutcome>> {
    if (!canonicalUuid.test(conversationId) || workspace.conversationId !== conversationId) {
      return Promise.resolve(invalidConversationId())
    }
    const body: SessionWorkspaceWriteRequestBody = { basedRevision, workspace }
    return postJson(`/conversations/${encodeURIComponent(conversationId)}/workspace`, body)
  },

  resolveRecovery(request: ResolveRecoveryItemRequest): Promise<IpcResult<RecoveryActionResult>> {
    let parsed: ResolveRecoveryItemRequest
    try {
      parsed = parseResolveRecoveryItemRequest(request)
    } catch (error) {
      return Promise.resolve({
        success: false,
        error: error instanceof Error ? error.message : String(error),
        code: 'VALIDATION_ERROR'
      })
    }
    return postJson('/conversation-recovery/resolve', parsed)
  }
}
