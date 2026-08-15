import type { ConversationId } from '@shared/types/conversation.types'
import type {
  ConversationLifecycleApi,
  ConversationLifecycleErrorCode,
  ConversationLifecycleOutcome,
  ConversationReplacementRequest
} from '@shared/types/conversation-lifecycle.types'
import type { IpcResult } from '@shared/types/ipc.types'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AcpTransportError, getAcpTransport } from './acp-transport'
import { isTauriContext } from './tauri-runtime'

const canonicalUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{12}$/

type IpcBody<T> = { success: true; data?: T } | { success: false; error: string; code: string }

export class ConversationLifecycleApiError extends Error {
  readonly code: ConversationLifecycleErrorCode

  constructor(code: ConversationLifecycleErrorCode, message: string) {
    super(message)
    this.name = 'ConversationLifecycleApiError'
    this.code = code
  }
}

function invalidConversationId(): never {
  throw new ConversationLifecycleApiError(
    'VALIDATION_ERROR',
    'conversationId must be a canonical lowercase-hyphenated UUID'
  )
}

function assertRequest(
  conversationId: ConversationId,
  expectedRevision: number,
  request?: ConversationReplacementRequest
): void {
  if (!canonicalUuid.test(conversationId)) invalidConversationId()
  if (!Number.isSafeInteger(expectedRevision) || expectedRevision < 0) {
    throw new ConversationLifecycleApiError(
      'VALIDATION_ERROR',
      'expectedRevision must be a non-negative safe integer'
    )
  }
  if (request && request.conversationId !== conversationId) invalidConversationId()
}

function unwrap<T>(result: IpcResult<T>): T {
  if (result.success) return result.data
  throw new ConversationLifecycleApiError(
    result.code as ConversationLifecycleErrorCode,
    result.error
  )
}

async function tauriMutation(
  command: string,
  conversationId: ConversationId,
  expectedRevision: number,
  request?: ConversationReplacementRequest
): Promise<ConversationLifecycleOutcome> {
  assertRequest(conversationId, expectedRevision, request)
  const result = await invoke<IpcResult<ConversationLifecycleOutcome>>(command, {
    conversationId,
    expectedRevision,
    ...(request ? { request } : {})
  })
  return unwrap(result)
}

function serverBase(): string {
  if (typeof window === 'undefined' || !window.location) return ''
  return window.location.origin
}

async function httpMutation(
  action: 'detach' | 'rebind' | 'suspend' | 'replace' | 'delete',
  conversationId: ConversationId,
  expectedRevision: number,
  request?: ConversationReplacementRequest
): Promise<ConversationLifecycleOutcome> {
  const response = await fetch(
    `${serverBase()}/conversations/${encodeURIComponent(conversationId)}/lifecycle/${action}`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ expectedRevision, ...(request ? { request } : {}) })
    }
  )
  if (!response.ok) {
    throw new ConversationLifecycleApiError(
      'NETWORK_ERROR',
      `HTTP ${response.status} ${response.statusText}`
    )
  }
  const body = (await response.json()) as IpcBody<ConversationLifecycleOutcome>
  if (body.success) return body.data as ConversationLifecycleOutcome
  throw new ConversationLifecycleApiError(body.code as ConversationLifecycleErrorCode, body.error)
}

async function webMutation(
  action: 'detach' | 'rebind' | 'suspend' | 'replace' | 'delete',
  conversationId: ConversationId,
  expectedRevision: number,
  request?: ConversationReplacementRequest
): Promise<ConversationLifecycleOutcome> {
  assertRequest(conversationId, expectedRevision, request)
  try {
    const transport = getAcpTransport()
    if (transport.conversationLifecycle) {
      return await transport.conversationLifecycle(
        action,
        conversationId,
        expectedRevision,
        request
      )
    }
    return await httpMutation(action, conversationId, expectedRevision, request)
  } catch (error) {
    if (error instanceof ConversationLifecycleApiError) throw error
    if (error instanceof AcpTransportError) {
      throw new ConversationLifecycleApiError(
        error.code as ConversationLifecycleErrorCode,
        error.message
      )
    }
    throw new ConversationLifecycleApiError(
      'NETWORK_ERROR',
      error instanceof Error ? error.message : String(error)
    )
  }
}

export const conversationLifecycleApi: ConversationLifecycleApi = {
  detachBinding(conversationId, expectedRevision) {
    return isTauriContext()
      ? tauriMutation('conversation_detach_binding', conversationId, expectedRevision)
      : webMutation('detach', conversationId, expectedRevision)
  },

  rebindDetachedBinding(conversationId, expectedRevision) {
    return isTauriContext()
      ? tauriMutation('conversation_rebind_detached_binding', conversationId, expectedRevision)
      : webMutation('rebind', conversationId, expectedRevision)
  },

  suspendBinding(conversationId, expectedRevision) {
    return isTauriContext()
      ? tauriMutation('conversation_suspend_binding', conversationId, expectedRevision)
      : webMutation('suspend', conversationId, expectedRevision)
  },

  replaceBinding(conversationId, request, expectedRevision) {
    return isTauriContext()
      ? tauriMutation('conversation_replace_binding', conversationId, expectedRevision, request)
      : webMutation('replace', conversationId, expectedRevision, request)
  },

  deleteConversation(conversationId, expectedRevision) {
    return isTauriContext()
      ? tauriMutation('conversation_delete', conversationId, expectedRevision)
      : webMutation('delete', conversationId, expectedRevision)
  },

  subscribe(listener) {
    if (isTauriContext()) {
      let resolved: UnlistenFn | null = null
      let cancelled = false
      void listen<ConversationLifecycleOutcome>('conversation:lifecycle', (event) => {
        listener(event.payload)
      }).then((unlisten) => {
        if (cancelled) unlisten()
        else resolved = unlisten
      })
      return () => {
        cancelled = true
        resolved?.()
        resolved = null
      }
    }
    return getAcpTransport().onEvent<ConversationLifecycleOutcome>(
      'conversation_lifecycle',
      listener
    )
  }
}
