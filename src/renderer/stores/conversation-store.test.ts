import type {
  ConversationAggregateMutationAction,
  ConversationAggregateMutationOutcome,
  ConversationRecordV2,
  ExecutionTarget,
  ProjectAttachment
} from '@shared/types/conversation.types'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { conversationApi } from '@/lib/conversation-api'
import { useAcpStore } from '@/stores/acp-store'
import { selectVisibleConversations, useConversationStore } from '@/stores/conversation-store'

vi.mock('@/lib/conversation-api', () => ({
  conversationApi: {
    listConversations: vi.fn(),
    openConversation: vi.fn(),
    attachProject: vi.fn(),
    detachProject: vi.fn(),
    updateExecutionTarget: vi.fn()
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
const attachment: ProjectAttachment = {
  schemaVersion: 1,
  projectId: 'project-1',
  attachedAtUtc: '2026-08-15T10:15:00.000Z',
  projectPathSnapshot: '/projects/attached',
  worktreePath: null,
  worktreeBranch: null
}

function aggregateOutcome(
  current: ConversationRecordV2,
  action: ConversationAggregateMutationAction,
  projectAttachment: ProjectAttachment | null,
  executionTarget: ExecutionTarget
): ConversationAggregateMutationOutcome {
  const conversation = {
    ...current,
    projectAttachment,
    executionTarget,
    lastSeq: current.lastSeq + 1
  }
  const identity = {
    conversationId: current.conversationId,
    createdAtUtc: current.createdAtUtc,
    creationPartition: current.creationPartition,
    workspaceCwd: current.workspaceCwd
  }
  return {
    status: 'updated',
    action,
    conversationId: current.conversationId,
    previousRevision: current.lastSeq,
    revision: conversation.lastSeq,
    identityBefore: identity,
    identityAfter: identity,
    projectAttachment,
    executionTarget,
    conversation
  }
}

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

  it('uses the current revision and applies attach, retarget, workspace, and detach atomically', async () => {
    useConversationStore.getState().replaceSummaries([projectless])
    const attachedOutcome = aggregateOutcome(
      projectless,
      'attachProject',
      attachment,
      projectless.executionTarget
    )
    const target: ExecutionTarget = {
      kind: 'project_root',
      projectId: attachment.projectId,
      projectRoot: attachment.projectPathSnapshot
    }
    const targetedOutcome = aggregateOutcome(
      attachedOutcome.conversation,
      'updateExecutionTarget',
      attachment,
      target
    )
    const workspaceOutcome = aggregateOutcome(
      targetedOutcome.conversation,
      'updateExecutionTarget',
      attachment,
      { kind: 'workspace' }
    )
    const detachedOutcome = aggregateOutcome(workspaceOutcome.conversation, 'detachProject', null, {
      kind: 'workspace'
    })
    vi.mocked(conversationApi.attachProject).mockResolvedValue({
      success: true,
      data: attachedOutcome
    })
    vi.mocked(conversationApi.updateExecutionTarget)
      .mockResolvedValueOnce({ success: true, data: targetedOutcome })
      .mockResolvedValueOnce({ success: true, data: workspaceOutcome })
    vi.mocked(conversationApi.detachProject).mockResolvedValue({
      success: true,
      data: detachedOutcome
    })

    await expect(
      useConversationStore.getState().attachProject(projectlessId, attachment)
    ).resolves.toEqual(attachedOutcome)
    await expect(
      useConversationStore.getState().updateExecutionTarget(projectlessId, target)
    ).resolves.toEqual(targetedOutcome)
    await expect(
      useConversationStore.getState().updateExecutionTarget(projectlessId, { kind: 'workspace' })
    ).resolves.toEqual(workspaceOutcome)
    await expect(useConversationStore.getState().detachProject(projectlessId)).resolves.toEqual(
      detachedOutcome
    )

    expect(conversationApi.attachProject).toHaveBeenCalledWith(projectlessId, 4, attachment)
    expect(conversationApi.updateExecutionTarget).toHaveBeenNthCalledWith(
      1,
      projectlessId,
      5,
      target
    )
    expect(conversationApi.updateExecutionTarget).toHaveBeenNthCalledWith(2, projectlessId, 6, {
      kind: 'workspace'
    })
    expect(conversationApi.detachProject).toHaveBeenCalledWith(projectlessId, 7)
    const finalRecord = useConversationStore.getState().summariesById[projectlessId]
    expect(finalRecord).toEqual(detachedOutcome.conversation)
    expect(finalRecord.workspaceCwd).toBe(projectless.workspaceCwd)
    expect(finalRecord.createdAtUtc).toBe(projectless.createdAtUtc)
    expect(useConversationStore.getState().aggregateBusyById[projectlessId]).toBe(false)
  })

  it('fails closed when a host aggregate outcome changes immutable identity', async () => {
    useConversationStore.getState().replaceSummaries([projectless])
    const invalid = aggregateOutcome(
      projectless,
      'attachProject',
      attachment,
      projectless.executionTarget
    )
    invalid.conversation = {
      ...invalid.conversation,
      workspaceCwd: '/unexpected/changed-workspace'
    }
    vi.mocked(conversationApi.attachProject).mockResolvedValue({ success: true, data: invalid })

    await expect(
      useConversationStore.getState().attachProject(projectlessId, attachment)
    ).resolves.toBeNull()
    expect(useConversationStore.getState().summariesById[projectlessId]).toEqual(projectless)
    expect(useConversationStore.getState().errorsById[projectlessId]?.code).toBe(
      'CONVERSATION_IDENTITY_CHANGED'
    )
  })
})
