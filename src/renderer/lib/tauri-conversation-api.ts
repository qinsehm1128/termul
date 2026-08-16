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
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export function normalizeConversationError(error: unknown): IpcResult<never> {
  if (error && typeof error === 'object') {
    const record = error as Record<string, unknown>
    if (typeof record.code === 'string') {
      return {
        success: false,
        code: record.code,
        error:
          typeof record.error === 'string'
            ? record.error
            : typeof record.message === 'string'
              ? record.message
              : record.code
      }
    }
  }
  return {
    success: false,
    code: 'INVOKE_ERROR',
    error: error instanceof Error ? error.message : String(error)
  }
}

function invalidConversationId(): IpcResult<never> {
  return {
    success: false,
    code: 'CONVERSATION_INVALID_ID',
    error: 'conversationId must be a canonical lowercase-hyphenated UUID'
  }
}

async function invokeConversation<T>(
  command: string,
  args?: Record<string, unknown>
): Promise<IpcResult<T>> {
  try {
    return await invoke<IpcResult<T>>(command, args)
  } catch (error) {
    return normalizeConversationError(error)
  }
}

function withConversationId<T>(
  conversationId: ConversationId,
  operation: () => Promise<IpcResult<T>>
): Promise<IpcResult<T>> {
  return isConversationId(conversationId) ? operation() : Promise.resolve(invalidConversationId())
}

export function createTauriConversationApi(): ConversationApi {
  return {
    getHostStatus: () => invokeConversation<ConversationHostStatus>('conversation_host_status'),
    listConversations: () => invokeConversation<ConversationRecordV2[]>('conversation_list'),
    getConversation: (conversationId) =>
      withConversationId(conversationId, () =>
        invokeConversation<ConversationRecordV2>('conversation_get', { conversationId })
      ),
    openConversation: (conversationId) =>
      withConversationId(conversationId, () =>
        invokeConversation<ConversationOpenOutcome>('conversation_open', { conversationId })
      ),
    resolveLegacyConversationId: (request: LegacyConversationKey) => {
      if (!request.value.trim()) {
        return Promise.resolve({
          success: false,
          code: 'VALIDATION_ERROR',
          error: 'legacy value must be non-empty'
        })
      }
      return invokeConversation<LegacyConversationResolution>('conversation_resolve_legacy_id', {
        request
      })
    },
    subscribeHostStatus(listener) {
      let active = true
      let unlisten: (() => void) | undefined
      void listen('conversation:host-status', listener).then((dispose) => {
        if (active) unlisten = dispose
        else dispose()
      })
      return () => {
        active = false
        unlisten?.()
      }
    }
  }
}

/** Exact core singleton selected by the production Conversation facade on Tauri. */
export const tauriConversationApi = createTauriConversationApi()
