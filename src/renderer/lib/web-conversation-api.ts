import type { ConversationId, ConversationRecordV2 } from '@shared/types/conversation.types'
import type {
  ConversationApi,
  ConversationApplicationRequestType,
  ConversationHostStatus,
  ConversationOpenOutcome,
  LegacyConversationKey,
  LegacyConversationResolution
} from '@shared/types/conversation-api.types'
import type {
  ConversationLifecycleOutcome,
  ConversationReplacementRequest
} from '@shared/types/conversation-lifecycle.types'
import {
  parseResolveRecoveryItemRequest,
  type RecoveryActionResult,
  type ResolveRecoveryItemRequest
} from '@shared/types/conversation-recovery.types'
import type { IpcResult } from '@shared/types/ipc.types'
import type {
  SessionWorkspaceLoadOutcome,
  SessionWorkspaceWriteOutcome
} from '@shared/types/session-workspace.types'
import { AcpTransportError, getAcpTransport } from './acp-transport'

const canonicalUuid = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/

function serverBase(): string {
  return typeof window === 'undefined' ? '' : window.location.origin
}

function isLoopbackHost(): boolean {
  if (typeof window === 'undefined') return true
  return ['127.0.0.1', 'localhost', '::1', '[::1]'].includes(window.location.hostname)
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

function notifyHostStatusChanged(): void {
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new Event('conversation-host-status'))
  }
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
    const response = await fetch(`${serverBase()}${path}`, init)
    if (!response.ok)
      return failure('NETWORK_ERROR', `HTTP ${response.status} ${response.statusText}`)
    const body = (await response.json()) as IpcResult<T>
    return body
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

async function wsRequest<T>(
  type: ConversationApplicationRequestType,
  payload: unknown
): Promise<IpcResult<T>> {
  const transport = getAcpTransport()
  if (!transport.conversationRequest) {
    return failure(
      'CONVERSATION_SERVICE_UNAVAILABLE',
      'authenticated Conversation WebSocket is unavailable'
    )
  }
  try {
    return { success: true, data: await transport.conversationRequest<T>(type, payload) }
  } catch (error) {
    return normalizeWebError(error)
  }
}

function withConversationId<T>(
  conversationId: ConversationId,
  operation: () => Promise<IpcResult<T>>
): Promise<IpcResult<T>> {
  return canonicalUuid.test(conversationId) ? operation() : Promise.resolve(invalidConversationId())
}

async function lifecycleMutation(
  action: 'detach' | 'rebind' | 'suspend' | 'replace' | 'delete',
  conversationId: ConversationId,
  expectedRevision: number,
  request?: ConversationReplacementRequest
): Promise<IpcResult<ConversationLifecycleOutcome>> {
  if (!canonicalUuid.test(conversationId)) return invalidConversationId()
  if (isLoopbackHost()) {
    const routeAction = action === 'rebind' ? 'rebind' : action
    return postJson(
      `/conversations/${encodeURIComponent(conversationId)}/lifecycle/${routeAction}`,
      {
        expectedRevision,
        ...(request ? { request } : {})
      }
    )
  }
  const transport = getAcpTransport()
  if (!transport.conversationLifecycle) {
    return failure(
      'CONVERSATION_SERVICE_UNAVAILABLE',
      'authenticated Conversation WebSocket is unavailable'
    )
  }
  try {
    return {
      success: true,
      data: await transport.conversationLifecycle(action, conversationId, expectedRevision, request)
    }
  } catch (error) {
    return normalizeWebError(error)
  }
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
    getWorkspace: (conversationId) =>
      withConversationId(conversationId, () =>
        requestJson<SessionWorkspaceLoadOutcome>(
          `/conversations/${encodeURIComponent(conversationId)}/workspace`
        )
      ),
    writeWorkspace: (conversationId, basedRevision, workspace) => {
      if (!canonicalUuid.test(conversationId) || workspace.conversationId !== conversationId) {
        return Promise.resolve(invalidConversationId())
      }
      const payload = { conversationId, basedRevision, workspace }
      return isLoopbackHost()
        ? postJson(`/conversations/${encodeURIComponent(conversationId)}/workspace`, {
            basedRevision,
            workspace
          })
        : wsRequest<SessionWorkspaceWriteOutcome>('write_session_workspace', payload)
    },
    resolveRecovery: async (request: ResolveRecoveryItemRequest) => {
      let parsed: ResolveRecoveryItemRequest
      try {
        parsed = parseResolveRecoveryItemRequest(request)
      } catch (error) {
        return failure('VALIDATION_ERROR', error instanceof Error ? error.message : String(error))
      }
      const outcome =
        parsed.action === 'inspect' || isLoopbackHost()
          ? await postJson<RecoveryActionResult>('/conversation-recovery/resolve', parsed)
          : await wsRequest<RecoveryActionResult>('resolve_recovery_item', parsed)
      if (outcome.success) notifyHostStatusChanged()
      return outcome
    },
    detachBinding: (conversationId, expectedRevision) =>
      lifecycleMutation('detach', conversationId, expectedRevision),
    rebindDetachedBinding: (conversationId, expectedRevision) =>
      lifecycleMutation('rebind', conversationId, expectedRevision),
    suspendBinding: (conversationId, expectedRevision) =>
      lifecycleMutation('suspend', conversationId, expectedRevision),
    replaceBinding: (conversationId, request, expectedRevision) =>
      lifecycleMutation('replace', conversationId, expectedRevision, request),
    deleteConversation: (conversationId, expectedRevision) =>
      lifecycleMutation('delete', conversationId, expectedRevision),
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

export const webConversationApi = createWebConversationApi()
