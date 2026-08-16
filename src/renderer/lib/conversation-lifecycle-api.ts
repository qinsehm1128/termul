import { type ConversationId, parseConversationId } from '@shared/types/conversation.types'
import type {
  AcpCompensationFailure,
  ConversationLifecycleApi,
  ConversationLifecycleErrorCode,
  ConversationLifecycleOutcome,
  ConversationReplacementRequest
} from '@shared/types/conversation-lifecycle.types'
import type { IpcResult } from '@shared/types/ipc.types'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AcpTransportError, getAcpTransport, remoteAccessHeaders } from './acp-transport'
import { isTauriContext } from './tauri-runtime'

type IpcBody<T> = { success: true; data?: T } | { success: false; error: string; code: string }
type ConversationLifecycleRuntime = 'tauri' | 'web'

function parseCompensationFailure(message: string): AcpCompensationFailure | null {
  try {
    const value = JSON.parse(message) as Partial<AcpCompensationFailure>
    if (
      typeof value.conversationId !== 'string' ||
      typeof value.primaryCode !== 'string' ||
      (value.providerCloseCode !== undefined && typeof value.providerCloseCode !== 'string') ||
      (value.failureRecordCode !== undefined && typeof value.failureRecordCode !== 'string') ||
      (value.recoveryMarkerCode !== undefined && typeof value.recoveryMarkerCode !== 'string') ||
      (value.recoveryRecordCode !== undefined && typeof value.recoveryRecordCode !== 'string') ||
      (value.recoveryId !== undefined && typeof value.recoveryId !== 'string')
    ) {
      return null
    }
    return value as AcpCompensationFailure
  } catch {
    return null
  }
}

export class ConversationLifecycleApiError extends Error {
  readonly code: ConversationLifecycleErrorCode
  readonly compensation: AcpCompensationFailure | null

  constructor(code: ConversationLifecycleErrorCode, message: string) {
    super(message)
    this.name = 'ConversationLifecycleApiError'
    this.code = code
    this.compensation =
      code === 'ACP_COMPENSATION_FAILED' ? parseCompensationFailure(message) : null
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
  try {
    parseConversationId(conversationId)
  } catch {
    invalidConversationId()
  }
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
      headers: remoteAccessHeaders({ 'content-type': 'application/json' }),
      body: JSON.stringify({ expectedRevision, ...(request ? { request } : {}) })
    }
  )
  if (!response.ok) {
    let body: IpcBody<ConversationLifecycleOutcome> | null = null
    try {
      body = (await response.json()) as IpcBody<ConversationLifecycleOutcome>
    } catch {
      // Preserve the generic network failure when the response has no application envelope.
    }
    if (body && !body.success) {
      throw new ConversationLifecycleApiError(
        body.code as ConversationLifecycleErrorCode,
        body.error
      )
    }
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

/** Build the same lifecycle stack used by the production singleton. */
export function createConversationLifecycleApi(
  runtime: ConversationLifecycleRuntime = isTauriContext() ? 'tauri' : 'web'
): ConversationLifecycleApi {
  const useTauri = runtime === 'tauri'
  return {
    detachBinding(conversationId, expectedRevision) {
      return useTauri
        ? tauriMutation('conversation_detach_binding', conversationId, expectedRevision)
        : webMutation('detach', conversationId, expectedRevision)
    },

    rebindDetachedBinding(conversationId, expectedRevision) {
      return useTauri
        ? tauriMutation('conversation_rebind_detached_binding', conversationId, expectedRevision)
        : webMutation('rebind', conversationId, expectedRevision)
    },

    suspendBinding(conversationId, expectedRevision) {
      return useTauri
        ? tauriMutation('conversation_suspend_binding', conversationId, expectedRevision)
        : webMutation('suspend', conversationId, expectedRevision)
    },

    replaceBinding(conversationId, request, expectedRevision) {
      return useTauri
        ? tauriMutation('conversation_replace_binding', conversationId, expectedRevision, request)
        : webMutation('replace', conversationId, expectedRevision, request)
    },

    deleteConversation(conversationId, expectedRevision) {
      return useTauri
        ? tauriMutation('conversation_delete', conversationId, expectedRevision)
        : webMutation('delete', conversationId, expectedRevision)
    },

    subscribe(listener) {
      if (useTauri) {
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
}

/** Exact lifecycle singleton imported by production hooks and stores. */
export const conversationLifecycleApi = createConversationLifecycleApi()
