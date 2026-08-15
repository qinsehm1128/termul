import type { ConversationLifecycleOutcome } from '@shared/types/conversation-lifecycle.types'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const { closeViewSpy, remapViewSpy, detachSpy, rebindSpy, suspendSpy, replaceSpy, deleteSpy } =
  vi.hoisted(() => ({
    closeViewSpy: vi.fn(),
    remapViewSpy: vi.fn(),
    detachSpy: vi.fn(),
    rebindSpy: vi.fn(),
    suspendSpy: vi.fn(),
    replaceSpy: vi.fn(),
    deleteSpy: vi.fn()
  }))

vi.mock('@/stores/workspace-store', () => ({
  useWorkspaceStore: {
    getState: () => ({
      closeChatView: closeViewSpy,
      remapAgentChatSession: remapViewSpy,
      addAgentChatTab: vi.fn(),
      getActiveTab: vi.fn(() => undefined)
    })
  }
}))

vi.mock('@/lib/conversation-lifecycle-api', () => {
  class ConversationLifecycleApiError extends Error {
    code: string
    constructor(code: string, message: string) {
      super(message)
      this.code = code
    }
  }
  return {
    ConversationLifecycleApiError,
    conversationLifecycleApi: {
      detachBinding: detachSpy,
      rebindDetachedBinding: rebindSpy,
      suspendBinding: suspendSpy,
      replaceBinding: replaceSpy,
      deleteConversation: deleteSpy,
      subscribe: vi.fn(() => vi.fn())
    }
  }
})

vi.mock('@/lib/acp-history-persistence', async (importActual) => {
  const actual = await importActual<typeof import('@/lib/acp-history-persistence')>()
  return {
    ...actual,
    loadSessionIndex: vi.fn(async () => []),
    saveSessionIndex: vi.fn(async () => {}),
    queueSessionPayloadSave: vi.fn(async () => {})
  }
})
vi.mock('@/lib/log-api', () => ({ logFrontendError: vi.fn() }))
vi.mock('@/lib/tauri-runtime', () => ({ isTauriContext: () => true }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))

import { useAcpStore } from './acp-store'

const conversationId = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'

function updated(
  action: 'detachBinding' | 'rebindDetachedBinding' | 'suspendBinding' | 'replaceBinding',
  state: 'active' | 'detached' | 'suspended',
  sessionId = 'session-old',
  revision = 5
): ConversationLifecycleOutcome {
  return {
    status: 'updated',
    action,
    conversationId,
    previousRevision: revision - 1,
    revision,
    workspaceCwd: '/visible/conversation',
    lifecycleState: 'ready',
    currentBinding: {
      schemaVersion: 1,
      bindingId: 'b2832b54-2ca4-4db4-93fd-f93bf6793114',
      agentSessionId: sessionId,
      runtimeAgentId: 'agent-1',
      stableAgentNamespace: 'config:test',
      executionCwd: '/visible/conversation',
      boundAtUtc: '2026-08-15T09:45:16.000Z',
      state
    }
  }
}

function seed(): void {
  useAcpStore.setState({
    sessionIndex: [
      {
        id: 'session-old',
        conversationId,
        agentId: 'agent-1',
        title: 'Lifecycle chat',
        cwd: '/visible/conversation',
        projectId: '',
        createdAt: 1,
        lastActivityAt: 2,
        messageCount: 1,
        lastSeq: 4,
        status: 'active'
      }
    ],
    sessions: {
      'session-old': {
        id: 'session-old',
        conversationId,
        agentId: 'agent-1',
        cwd: '/visible/conversation',
        projectId: '',
        status: 'active',
        title: 'Lifecycle chat',
        activeTurn: false,
        openTurnId: null,
        modes: null,
        configOptions: [],
        lastError: null,
        createdAt: 1
      }
    },
    activeSessionId: 'session-old',
    messages: {
      'session-old': [
        {
          id: 'message-1',
          role: 'user',
          blocks: [{ type: 'text', text: 'retained transcript' }],
          streaming: false,
          timestamp: 1
        }
      ]
    },
    toolCalls: { 'session-old': [] },
    plans: { 'session-old': [] },
    commands: { 'session-old': [] },
    sessionUsage: {},
    promptQueues: {},
    suppressQueueFlush: {},
    restoringChatIds: {},
    launchingSessionIds: {},
    degradedRecoverySessions: {},
    pendingPermissions: {},
    pendingQuestions: {}
  })
}

describe('ACP Conversation lifecycle store', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    seed()
  })

  it('closes only the renderer view and preserves session and transcript state', () => {
    useAcpStore.getState().closeChatView(conversationId)

    expect(closeViewSpy).toHaveBeenCalledWith('session-old')
    expect(detachSpy).not.toHaveBeenCalled()
    expect(suspendSpy).not.toHaveBeenCalled()
    expect(deleteSpy).not.toHaveBeenCalled()
    expect(useAcpStore.getState().sessions['session-old']).toBeDefined()
    expect(useAcpStore.getState().messages['session-old']?.[0].blocks[0]).toMatchObject({
      text: 'retained transcript'
    })
  })

  it('preserves Conversation transcript maps when provider close emits session_closed', () => {
    useAcpStore.getState()._onSessionClosed({ agentId: 'agent-1', sessionId: 'session-old' })

    expect(useAcpStore.getState().sessions['session-old']?.status).toBe('closed')
    expect(useAcpStore.getState().messages['session-old']).toHaveLength(1)
  })

  it('uses canonical lastSeq before detach, rebind, and suspend mutations', async () => {
    detachSpy.mockResolvedValue(updated('detachBinding', 'detached'))
    await useAcpStore.getState().detachAgentBinding(conversationId)
    expect(detachSpy).toHaveBeenCalledWith(conversationId, 4)

    useAcpStore.setState((state) => ({
      sessionIndex: state.sessionIndex.map((entry) => ({ ...entry, lastSeq: 5 }))
    }))
    rebindSpy.mockResolvedValue(updated('rebindDetachedBinding', 'active', 'session-old', 6))
    await useAcpStore.getState().rebindDetachedBinding(conversationId)
    expect(rebindSpy).toHaveBeenCalledWith(conversationId, 5)

    suspendSpy.mockResolvedValue(updated('suspendBinding', 'suspended', 'session-old', 7))
    await useAcpStore.getState().suspendAgentBinding(conversationId)
    expect(suspendSpy).toHaveBeenCalledWith(conversationId, 6)
    expect(useAcpStore.getState().messages['session-old']).toHaveLength(1)
  })

  it('replaces the opaque session id while retaining Conversation identity and transcript maps', async () => {
    replaceSpy.mockResolvedValue(updated('replaceBinding', 'active', 'session-new'))

    await useAcpStore.getState().replaceAgentBinding(conversationId)

    expect(replaceSpy).toHaveBeenCalledWith(
      conversationId,
      expect.objectContaining({
        conversationId,
        executionTarget: { kind: 'workspace' }
      }),
      4
    )
    expect(useAcpStore.getState().sessions['session-old']).toBeUndefined()
    expect(useAcpStore.getState().sessions['session-new']?.conversationId).toBe(conversationId)
    expect(useAcpStore.getState().messages['session-new']).toHaveLength(1)
    expect(remapViewSpy).toHaveBeenCalledWith('session-old', 'session-new')
  })

  it('keeps state intact when delete is blocked by live bindings or terminals', async () => {
    deleteSpy.mockResolvedValue({
      status: 'blocked',
      action: 'deleteConversation',
      conversationId,
      revision: 4,
      code: 'CONVERSATION_LIVE_RESOURCES',
      blockers: [
        { kind: 'liveBinding', count: 1, ids: ['session-old'] },
        { kind: 'terminalResources', count: 1, ids: ['terminal-live'] }
      ]
    })

    const outcome = await useAcpStore.getState().deleteConversation(conversationId)

    expect(outcome.status).toBe('blocked')
    expect(useAcpStore.getState().sessionIndex).toHaveLength(1)
    expect(useAcpStore.getState().sessions['session-old']).toBeDefined()
    expect(closeViewSpy).not.toHaveBeenCalled()
  })

  it('applies explicit tombstone without invoking resource teardown', async () => {
    deleteSpy.mockResolvedValue({
      status: 'updated',
      action: 'deleteConversation',
      conversationId,
      previousRevision: 4,
      revision: 4,
      workspaceCwd: '/visible/conversation',
      lifecycleState: 'deleted',
      currentBinding: {
        ...updated('suspendBinding', 'suspended').currentBinding!,
        state: 'suspended'
      }
    })

    await useAcpStore.getState().deleteConversation(conversationId)

    expect(deleteSpy).toHaveBeenCalledWith(conversationId, 4)
    expect(useAcpStore.getState().sessionIndex).toEqual([])
    expect(useAcpStore.getState().sessions['session-old']).toBeUndefined()
    expect(closeViewSpy).toHaveBeenCalledWith('session-old')
  })
})
