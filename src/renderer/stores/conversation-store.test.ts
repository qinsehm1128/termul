import type { ConversationRecordV2 } from '@shared/types/conversation.types'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { conversationApi } from '@/lib/conversation-api'
import { useAcpStore } from '@/stores/acp-store'
import { selectVisibleConversations, useConversationStore } from '@/stores/conversation-store'

vi.mock('@/lib/conversation-api', () => ({
  conversationApi: {
    listConversations: vi.fn(),
    openConversation: vi.fn()
  }
}))

vi.mock('@/lib/log-api', () => ({ logFrontendError: vi.fn() }))

const projectlessId = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'
const attachedId = '028f7a1c-1b4d-7c8a-9f01-0123456789ab'

function summary(
  conversationId: string,
  workspaceCwd: string,
  projectId: string | null
): ConversationRecordV2 {
  return {
    schemaVersion: 2,
    conversationId,
    createdAtUtc:
      conversationId === projectlessId ? '2026-08-15T10:00:00.000Z' : '2026-08-15T09:00:00.000Z',
    creationPartition: { year: 2026, month: 8, day: 15, path: '2026/08/15' },
    workspaceCwd,
    executionTarget: { kind: 'workspace' },
    projectAttachment: projectId
      ? {
          schemaVersion: 1,
          projectId,
          attachedAtUtc: '2026-08-15T09:00:00.000Z',
          projectPathSnapshot: '/projects/attached',
          worktreePath: null,
          worktreeBranch: null
        }
      : null,
    lifecycleState: 'ready',
    lastSeq: 4,
    createdBy: 'termul'
  }
}

const projectless = summary(projectlessId, '/conversations/projectless', null)
const attached = summary(attachedId, '/conversations/attached', 'project-1')

beforeEach(() => {
  vi.clearAllMocks()
  useConversationStore.getState().reset()
  useAcpStore.setState({ activeSessionId: null, sessionIndex: [] })
})

describe('ConversationStore canonical authority', () => {
  it('loads project-less and attached summaries with zero selected projects', async () => {
    vi.mocked(conversationApi.listConversations).mockResolvedValue({
      success: true,
      data: [attached, projectless]
    })

    await expect(useConversationStore.getState().loadConversations()).resolves.toBe(true)
    const state = useConversationStore.getState()
    expect(state.conversationIds).toEqual([projectlessId, attachedId])
    expect(state.summariesById[projectlessId].projectAttachment).toBeNull()
    expect(state.summariesById[attachedId].projectAttachment?.projectId).toBe('project-1')
  })

  it('opens and activates only by canonical ConversationId without changing ACP active session', async () => {
    vi.mocked(conversationApi.openConversation).mockResolvedValue({
      success: true,
      data: {
        conversation: projectless,
        workspace: { status: 'missing', conversationId: projectlessId }
      }
    })

    await expect(
      useConversationStore.getState().openConversation(projectlessId)
    ).resolves.toMatchObject({ conversation: { conversationId: projectlessId } })
    expect(useConversationStore.getState().activeConversationId).toBe(projectlessId)
    expect(useConversationStore.getState().detailsById[projectlessId]?.conversation).toEqual(
      projectless
    )
    expect(useAcpStore.getState().activeSessionId).toBeNull()

    useAcpStore.setState({ activeSessionId: 'opaque-runtime-session' })
    expect(useConversationStore.getState().activeConversationId).toBe(projectlessId)
    expect(useConversationStore.getState().conversationIds).toEqual([projectlessId])
  })

  it('rejects an opaque ACP session id without using it as a store key', async () => {
    await expect(
      useConversationStore.getState().openConversation('opaque/acp-session')
    ).resolves.toBeNull()
    expect(conversationApi.openConversation).not.toHaveBeenCalled()
    expect(useConversationStore.getState().conversationIds).toEqual([])
    expect(useConversationStore.getState().activeConversationId).toBeNull()
  })

  it('applies optional search/project filters without mutating attachment or cwd invariants', () => {
    useConversationStore.getState().replaceSummaries([projectless, attached])
    const before = structuredClone(useConversationStore.getState().summariesById)

    useConversationStore.getState().setProjectFilter('project-1')
    expect(selectVisibleConversations(useConversationStore.getState())).toEqual([attached])
    useConversationStore.getState().setProjectFilter(null)
    useConversationStore.getState().setSearchQuery('projectless')
    expect(selectVisibleConversations(useConversationStore.getState())).toEqual([projectless])
    expect(useConversationStore.getState().summariesById).toEqual(before)
  })

  it('keeps stable not-found and recovery-required errors for retry UI', async () => {
    vi.mocked(conversationApi.openConversation)
      .mockResolvedValueOnce({
        success: false,
        code: 'CONVERSATION_NOT_FOUND',
        error: 'missing'
      })
      .mockResolvedValueOnce({
        success: false,
        code: 'CONVERSATION_RECOVERY_REQUIRED',
        error: 'recover'
      })

    await useConversationStore.getState().openConversation(projectlessId)
    expect(useConversationStore.getState().errorsById[projectlessId]?.code).toBe(
      'CONVERSATION_NOT_FOUND'
    )
    await useConversationStore.getState().openConversation(projectlessId)
    expect(useConversationStore.getState().errorsById[projectlessId]?.code).toBe(
      'CONVERSATION_RECOVERY_REQUIRED'
    )
  })
})
