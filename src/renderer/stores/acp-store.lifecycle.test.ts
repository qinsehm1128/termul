import type { ConversationLifecycleOutcome } from '@shared/types/conversation-lifecycle.types'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const { closeViewSpy, remapViewSpy, invokeSpy } = vi.hoisted(() => ({
  closeViewSpy: vi.fn(),
  remapViewSpy: vi.fn(),
  invokeSpy: vi.fn()
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
vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeSpy }))
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

    expect(closeViewSpy).toHaveBeenCalledWith(conversationId)
    expect(invokeSpy).not.toHaveBeenCalled()
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

  it('dispatches detach, rebind, and suspend through the real production lifecycle factory', async () => {
    invokeSpy.mockResolvedValueOnce({ success: true, data: updated('detachBinding', 'detached') })
    await useAcpStore.getState().detachAgentBinding(conversationId)
    expect(invokeSpy).toHaveBeenNthCalledWith(1, 'conversation_detach_binding', {
      conversationId,
      expectedRevision: 4
    })

    useAcpStore.setState((state) => ({
      sessionIndex: state.sessionIndex.map((entry) => ({ ...entry, lastSeq: 5 }))
    }))
    invokeSpy.mockResolvedValueOnce({
      success: true,
      data: updated('rebindDetachedBinding', 'active', 'session-old', 6)
    })
    await useAcpStore.getState().rebindDetachedBinding(conversationId)
    expect(invokeSpy).toHaveBeenNthCalledWith(2, 'conversation_rebind_detached_binding', {
      conversationId,
      expectedRevision: 5
    })

    invokeSpy.mockResolvedValueOnce({
      success: true,
      data: updated('suspendBinding', 'suspended', 'session-old', 7)
    })
    await useAcpStore.getState().suspendAgentBinding(conversationId)
    expect(invokeSpy).toHaveBeenNthCalledWith(3, 'conversation_suspend_binding', {
      conversationId,
      expectedRevision: 6
    })
    expect(useAcpStore.getState().messages['session-old']).toHaveLength(1)
  })

  it('dispatches replace through the real factory while retaining identity and transcript maps', async () => {
    invokeSpy.mockResolvedValueOnce({
      success: true,
      data: updated('replaceBinding', 'active', 'session-new')
    })

    await useAcpStore.getState().replaceAgentBinding(conversationId)

    expect(invokeSpy).toHaveBeenCalledWith(
      'conversation_replace_binding',
      expect.objectContaining({
        conversationId,
        expectedRevision: 4,
        request: expect.objectContaining({
          conversationId,
          executionTarget: { kind: 'workspace' }
        })
      })
    )
    expect(useAcpStore.getState().sessions['session-old']).toBeUndefined()
    expect(useAcpStore.getState().sessions['session-new']?.conversationId).toBe(conversationId)
    expect(useAcpStore.getState().messages['session-new']).toHaveLength(1)
    expect(remapViewSpy).not.toHaveBeenCalled()
  })

  it('dispatches blocked delete through the real factory and keeps state intact', async () => {
    invokeSpy.mockResolvedValueOnce({
      success: true,
      data: {
        status: 'blocked',
        action: 'deleteConversation',
        conversationId,
        revision: 4,
        code: 'CONVERSATION_LIVE_RESOURCES',
        blockers: [
          { kind: 'liveBinding', count: 1, ids: ['session-old'] },
          { kind: 'terminalResources', count: 1, ids: ['terminal-live'] }
        ]
      }
    })

    const outcome = await useAcpStore.getState().deleteConversation(conversationId)

    expect(invokeSpy).toHaveBeenCalledWith('conversation_delete', {
      conversationId,
      expectedRevision: 4
    })
    expect(outcome.status).toBe('blocked')
    expect(useAcpStore.getState().sessionIndex).toHaveLength(1)
    expect(useAcpStore.getState().sessions['session-old']).toBeDefined()
    expect(closeViewSpy).not.toHaveBeenCalled()
  })

  it('applies explicit tombstone returned by the real delete route without resource teardown', async () => {
    invokeSpy.mockResolvedValueOnce({
      success: true,
      data: {
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
      }
    })

    await useAcpStore.getState().deleteConversation(conversationId)

    expect(invokeSpy).toHaveBeenCalledWith('conversation_delete', {
      conversationId,
      expectedRevision: 4
    })
    expect(useAcpStore.getState().sessionIndex).toEqual([])
    expect(useAcpStore.getState().sessions['session-old']).toBeUndefined()
    expect(closeViewSpy).toHaveBeenCalledWith(conversationId)
  })
})
