import {
  type ConversationId,
  type ConversationRecordV2,
  isConversationId
} from '@shared/types/conversation.types'
import type {
  ConversationApi,
  ConversationHostStatus,
  ConversationOpenOutcome,
  LegacyConversationKey,
  LegacyConversationResolution
} from '@shared/types/conversation-api.types'
import type { IpcResult } from '@shared/types/ipc.types'
import { AcpTransportError, remoteAccessHeaders } from './acp-transport'

function serverBase(): string {
  return typeof window === 'undefined' ? '' : window.location.origin
}

function failure(code: string, error: string): IpcResult<never> {
  return { success: false, code, error }
}

function normalizeWebError(error: unknown): IpcResult<never> {
  if (error instanceof AcpTransportError) return failure(error.code, error.message)
  if (error && typeof error === 'object') {
    const value = error as Record<string, unknown>
    if (typeof value.code === 'string') {
      return failure(
        value.code,
        typeof value.message === 'string' ? value.message : String(value.code)
      )
    }
  }
  return failure('NETWORK_ERROR', error instanceof Error ? error.message : String(error))
}

function invalidConversationId(): IpcResult<never> {
  return failure(
    'CONVERSATION_INVALID_ID',
    'conversationId must be a canonical lowercase-hyphenated UUID'
  )
}

async function requestJson<T>(
  path: string,
  init: RequestInit = { method: 'GET' }
): Promise<IpcResult<T>> {
  try {
    const response = await fetch(`${serverBase()}${path}`, {
      ...init,
      headers: remoteAccessHeaders(init.headers)
    })
    if (!response.ok) {
      try {
        const body = (await response.json()) as IpcResult<T>
        if (!body.success) return body
      } catch {
        // Preserve a generic network failure when no application envelope is available.
      }
      return failure('NETWORK_ERROR', `HTTP ${response.status} ${response.statusText}`)
    }
    return (await response.json()) as IpcResult<T>
  } catch (error) {
    return normalizeWebError(error)
  }
}

function postJson<T>(path: string, body: unknown): Promise<IpcResult<T>> {
  return requestJson(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body)
  })
}

function withConversationId<T>(
  conversationId: ConversationId,
  operation: () => Promise<IpcResult<T>>
): Promise<IpcResult<T>> {
  return isConversationId(conversationId) ? operation() : Promise.resolve(invalidConversationId())
}

export function createWebConversationApi(): ConversationApi {
  return {
    getHostStatus: () => requestJson<ConversationHostStatus>('/conversations/host-status'),
    listConversations: () => requestJson<ConversationRecordV2[]>('/conversations'),
    getConversation: (conversationId) =>
      withConversationId(conversationId, () =>
        requestJson<ConversationRecordV2>(`/conversations/${encodeURIComponent(conversationId)}`)
      ),
    openConversation: (conversationId) =>
      withConversationId(conversationId, () =>
        postJson<ConversationOpenOutcome>(
          `/conversations/${encodeURIComponent(conversationId)}/open`,
          {}
        )
      ),
    resolveLegacyConversationId: (request: LegacyConversationKey) => {
      if (!request.value.trim()) {
        return Promise.resolve(failure('VALIDATION_ERROR', 'legacy value must be non-empty'))
      }
      return postJson<LegacyConversationResolution>('/conversations/resolve-legacy', request)
    },
    subscribeHostStatus(listener) {
      if (typeof window === 'undefined') return () => undefined
      window.addEventListener('online', listener)
      window.addEventListener('visibilitychange', listener)
      window.addEventListener('conversation-host-status', listener)
      return () => {
        window.removeEventListener('online', listener)
        window.removeEventListener('visibilitychange', listener)
        window.removeEventListener('conversation-host-status', listener)
      }
    }
  }
}

/** Exact core singleton selected by the production Conversation facade on web. */
export const webConversationApi = createWebConversationApi()
