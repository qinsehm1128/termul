import type { ConversationId, ConversationRecordV2 } from '@shared/types/conversation.types'
import type {
  ConversationHostStatus,
  ConversationOpenOutcome
} from '@shared/types/conversation-api.types'
import type { RecoveryItemV1 } from '@shared/types/conversation-recovery.types'
import { create } from 'zustand'
import { useShallow } from 'zustand/shallow'
import { conversationApi } from '@/lib/conversation-api'
import { logFrontendError } from '@/lib/log-api'

const canonicalConversationIdPattern =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

export type ConversationProjectFilter = string | 'projectless' | null

export interface ConversationStoreError {
  code: string
  message: string
}

interface ConversationState {
  summariesById: Record<ConversationId, ConversationRecordV2>
  conversationIds: ConversationId[]
  detailsById: Record<ConversationId, ConversationOpenOutcome | undefined>
  recoveryItems: RecoveryItemV1[]
  activeConversationId: ConversationId | null
  searchQuery: string
  projectFilter: ConversationProjectFilter
  loadingList: boolean
  openingById: Record<ConversationId, boolean | undefined>
  errorsById: Record<ConversationId, ConversationStoreError | undefined>
  listError: ConversationStoreError | null
  replaceSummaries: (summaries: ConversationRecordV2[]) => void
  setRecoveryItems: (items: RecoveryItemV1[]) => void
  setSearchQuery: (query: string) => void
  setProjectFilter: (projectFilter: ConversationProjectFilter) => void
  setActiveConversationId: (conversationId: ConversationId | null) => void
  loadConversations: () => Promise<boolean>
  openConversation: (conversationId: ConversationId) => Promise<ConversationOpenOutcome | null>
  clearConversationError: (conversationId: ConversationId) => void
  reset: () => void
}

const initialState = {
  summariesById: {},
  conversationIds: [],
  detailsById: {},
  recoveryItems: [],
  activeConversationId: null,
  searchQuery: '',
  projectFilter: null,
  loadingList: false,
  openingById: {},
  errorsById: {},
  listError: null
} satisfies Pick<
  ConversationState,
  | 'summariesById'
  | 'conversationIds'
  | 'detailsById'
  | 'recoveryItems'
  | 'activeConversationId'
  | 'searchQuery'
  | 'projectFilter'
  | 'loadingList'
  | 'openingById'
  | 'errorsById'
  | 'listError'
>

export function isCanonicalConversationId(value: string): value is ConversationId {
  return canonicalConversationIdPattern.test(value)
}

function indexSummaries(summaries: ConversationRecordV2[]): {
  summariesById: Record<ConversationId, ConversationRecordV2>
  conversationIds: ConversationId[]
} {
  const summariesById: Record<ConversationId, ConversationRecordV2> = {}
  const conversationIds: ConversationId[] = []
  for (const summary of summaries) {
    if (!isCanonicalConversationId(summary.conversationId)) continue
    summariesById[summary.conversationId] = summary
    conversationIds.push(summary.conversationId)
  }
  conversationIds.sort((left, right) => {
    const leftCreated = summariesById[left]?.createdAtUtc ?? ''
    const rightCreated = summariesById[right]?.createdAtUtc ?? ''
    return rightCreated.localeCompare(leftCreated)
  })
  return { summariesById, conversationIds }
}

function stableError(code: string, message?: string): ConversationStoreError {
  return { code, message: message || code }
}

export const useConversationStore = create<ConversationState>((set) => ({
  ...initialState,

  replaceSummaries: (summaries) => set(indexSummaries(summaries)),

  setRecoveryItems: (recoveryItems) => set({ recoveryItems: [...recoveryItems] }),

  setSearchQuery: (searchQuery) => set({ searchQuery }),

  setProjectFilter: (projectFilter) => set({ projectFilter }),

  setActiveConversationId: (activeConversationId) => set({ activeConversationId }),

  loadConversations: async () => {
    set({ loadingList: true, listError: null })
    try {
      const result = await conversationApi.listConversations()
      if (!result.success) {
        set({ loadingList: false, listError: stableError(result.code, result.error) })
        void logFrontendError({
          level: 'warn',
          source: 'conversation-store.list',
          message: `code=${result.code}`
        })
        return false
      }
      set({ ...indexSummaries(result.data), loadingList: false, listError: null })
      return true
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      set({ loadingList: false, listError: stableError('CONVERSATION_LIST_FAILED', message) })
      void logFrontendError({
        level: 'warn',
        source: 'conversation-store.list',
        message: 'code=CONVERSATION_LIST_FAILED'
      })
      return false
    }
  },

  openConversation: async (conversationId) => {
    if (!isCanonicalConversationId(conversationId)) {
      const error = stableError(
        'CONVERSATION_INVALID_ID',
        'The Conversation address is not a canonical ConversationId.'
      )
      set((state) => ({
        errorsById: { ...state.errorsById, [conversationId]: error },
        openingById: { ...state.openingById, [conversationId]: false }
      }))
      void logFrontendError({
        level: 'warn',
        source: 'conversation-store.open',
        message: 'code=CONVERSATION_INVALID_ID'
      })
      return null
    }

    set((state) => ({
      openingById: { ...state.openingById, [conversationId]: true },
      errorsById: { ...state.errorsById, [conversationId]: undefined }
    }))
    try {
      const result = await conversationApi.openConversation(conversationId)
      if (!result.success) {
        set((state) => ({
          openingById: { ...state.openingById, [conversationId]: false },
          errorsById: {
            ...state.errorsById,
            [conversationId]: stableError(result.code, result.error)
          }
        }))
        void logFrontendError({
          level: 'warn',
          source: 'conversation-store.open',
          message: `conversationId=${conversationId} code=${result.code}`
        })
        return null
      }

      set((state) => {
        const summaryAlreadyListed = Boolean(state.summariesById[conversationId])
        return {
          summariesById: {
            ...state.summariesById,
            [conversationId]: result.data.conversation
          },
          conversationIds: summaryAlreadyListed
            ? state.conversationIds
            : [conversationId, ...state.conversationIds],
          detailsById: { ...state.detailsById, [conversationId]: result.data },
          openingById: { ...state.openingById, [conversationId]: false },
          errorsById: { ...state.errorsById, [conversationId]: undefined },
          activeConversationId: conversationId
        }
      })
      return result.data
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      set((state) => ({
        openingById: { ...state.openingById, [conversationId]: false },
        errorsById: {
          ...state.errorsById,
          [conversationId]: stableError('CONVERSATION_OPEN_FAILED', message)
        }
      }))
      void logFrontendError({
        level: 'warn',
        source: 'conversation-store.open',
        message: `conversationId=${conversationId} code=CONVERSATION_OPEN_FAILED`
      })
      return null
    }
  },

  clearConversationError: (conversationId) =>
    set((state) => ({
      errorsById: { ...state.errorsById, [conversationId]: undefined }
    })),

  reset: () => set(initialState)
}))

export function selectVisibleConversations(state: ConversationState): ConversationRecordV2[] {
  const query = state.searchQuery.trim().toLowerCase()
  return state.conversationIds
    .map((conversationId) => state.summariesById[conversationId])
    .filter((summary): summary is ConversationRecordV2 => Boolean(summary))
    .filter((summary) => {
      if (state.projectFilter === 'projectless') return summary.projectAttachment === null
      if (state.projectFilter) {
        return summary.projectAttachment?.projectId === state.projectFilter
      }
      return true
    })
    .filter((summary) => {
      if (!query) return true
      const attachment = summary.projectAttachment
      return [
        summary.conversationId,
        summary.workspaceCwd,
        attachment?.projectId,
        attachment?.projectPathSnapshot,
        attachment?.worktreePath,
        attachment?.worktreeBranch
      ].some((value) => value?.toLowerCase().includes(query))
    })
}

export function useVisibleConversations(): ConversationRecordV2[] {
  return useConversationStore(useShallow(selectVisibleConversations))
}

export function recoveryCountForConversation(
  recoveryItems: readonly RecoveryItemV1[],
  conversationId: ConversationId
): number {
  return recoveryItems.filter(
    (item) => item.status === 'unresolved' && item.conversationIds.includes(conversationId)
  ).length
}

export function applyConversationHostStatus(status: ConversationHostStatus): void {
  useConversationStore.getState().setRecoveryItems(status.recoveryItems)
}
