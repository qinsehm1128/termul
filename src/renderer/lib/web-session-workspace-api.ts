import { type ConversationId, isConversationId } from '@shared/types/conversation.types'
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
import { remoteAccessHeaders } from './acp-transport'
import { isTauriContext } from './tauri-runtime'

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
  let body: unknown
  try {
    body = await response.json()
  } catch (error) {
    return response.ok
      ? networkError(error instanceof Error ? error.message : 'invalid JSON')
      : networkError(`HTTP ${response.status} ${response.statusText}`)
  }

  if (typeof body === 'object' && body !== null && 'success' in body) {
    const envelope = body as {
      success?: unknown
      data?: unknown
      error?: unknown
      code?: unknown
    }
    if (response.ok && envelope.success === true) {
      return { success: true, data: envelope.data as T }
    }
    if (
      envelope.success === false &&
      typeof envelope.error === 'string' &&
      envelope.error.length > 0 &&
      typeof envelope.code === 'string' &&
      envelope.code.length > 0
    ) {
      return { success: false, error: envelope.error, code: envelope.code }
    }
  }

  return response.ok
    ? networkError('invalid response envelope')
    : networkError(`HTTP ${response.status} ${response.statusText}`)
}

async function getJson<T>(path: string): Promise<IpcResult<T>> {
  try {
    return await parseBody<T>(
      await fetch(`${serverBase()}${path}`, {
        method: 'GET',
        headers: remoteAccessHeaders()
      })
    )
  } catch (error) {
    return networkError(error instanceof Error ? error.message : String(error))
  }
}

async function postJson<T>(path: string, body: unknown): Promise<IpcResult<T>> {
  try {
    return await parseBody<T>(
      await fetch(`${serverBase()}${path}`, {
        method: 'POST',
        headers: remoteAccessHeaders({ 'content-type': 'application/json' }),
        body: JSON.stringify(body)
      })
    )
  } catch (error) {
    return networkError(error instanceof Error ? error.message : String(error))
  }
}

export const webSessionWorkspaceApi: SessionWorkspaceApi = {
  getWorkspace(conversationId: ConversationId): Promise<IpcResult<SessionWorkspaceLoadOutcome>> {
    if (!isConversationId(conversationId)) return Promise.resolve(invalidConversationId())
    return getJson(`/conversations/${encodeURIComponent(conversationId)}/workspace`)
  },

  writeWorkspace(
    conversationId: ConversationId,
    basedRevision: number | null,
    workspace: SessionWorkspaceV1
  ): Promise<IpcResult<SessionWorkspaceWriteOutcome>> {
    if (!isConversationId(conversationId) || workspace.conversationId !== conversationId) {
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
