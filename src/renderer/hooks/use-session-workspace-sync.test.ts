import { act, cleanup, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const { getMock, writeMock, recoveryMock, logMock } = vi.hoisted(() => ({
  getMock: vi.fn(),
  writeMock: vi.fn(),
  recoveryMock: vi.fn(),
  logMock: vi.fn()
}))

vi.mock('@/lib/session-workspace-api', () => ({
  sessionWorkspaceApi: {
    getWorkspace: getMock,
    writeWorkspace: writeMock,
    resolveRecovery: recoveryMock
  }
}))
vi.mock('@/lib/log-api', () => ({ logFrontendError: logMock }))
vi.mock('@/hooks/useTerminalAutoSave', () => ({ isTerminalRestoreInProgress: () => false }))

import type { SessionWorkspaceV1 } from '@shared/types/session-workspace.types'
import { useAcpStore } from '@/stores/acp-store'
import { useEditorStore } from '@/stores/editor-store'
import { useSessionWorkspaceSyncStore } from '@/stores/session-workspace-sync-store'
import { useTerminalStore } from '@/stores/terminal-store'
import { useWorkspaceStore } from '@/stores/workspace-store'
import {
  buildSessionWorkspace,
  getActiveConversationId,
  loadSessionWorkspace,
  performSessionWorkspaceWrite,
  resolveSessionWorkspaceConflict,
  resolveSessionWorkspaceRecovery,
  useSessionWorkspaceSync
} from './use-session-workspace-sync'

const one = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'
const two = '5f7a1c01-4d1b-4c8a-af01-0123456789ab'

function workspace(conversationId: string, revision: number, leafId: string): SessionWorkspaceV1 {
  return {
    schemaVersion: 1,
    conversationId,
    revision,
    updatedAtUtc: '2026-08-15T10:00:00.000Z',
    topology: {
      type: 'leaf',
      id: leafId,
      terminalIds: [],
      editorIds: [],
      activeTabId: null
    },
    activePaneId: leafId,
    resources: [],
    projectionState: { status: 'native' }
  }
}

beforeEach(() => {
  vi.clearAllMocks()
  useSessionWorkspaceSyncStore.setState({
    activeConversationId: null,
    basedRevisionByConversation: {},
    conflictsByConversation: {},
    recoveryByConversation: {},
    loadOutcomeByConversation: {},
    restoreInProgressByConversation: {}
  })
  useWorkspaceStore.getState().resetLayout()
  useAcpStore.setState({ sessions: {}, activeSessionId: null })
  useEditorStore.getState().clearAllFiles()
  useTerminalStore.setState({ terminals: [], activeTerminalId: '', ptyIdIndex: new Map() })
})

afterEach(() => {
  vi.useRealTimers()
  cleanup()
})

describe('Conversation-scoped SessionWorkspace sync', () => {
  it('restores two Conversations independently and keeps their revisions isolated', async () => {
    getMock
      .mockResolvedValueOnce({
        success: true,
        data: { status: 'loaded', workspace: workspace(one, 3, 'leaf-one') }
      })
      .mockResolvedValueOnce({
        success: true,
        data: { status: 'loaded', workspace: workspace(two, 8, 'leaf-two') }
      })

    await loadSessionWorkspace(one)
    expect(useWorkspaceStore.getState().root.id).toBe('leaf-one')
    await loadSessionWorkspace(two)
    expect(useWorkspaceStore.getState().root.id).toBe('leaf-two')
    const store = useSessionWorkspaceSyncStore.getState()
    expect(store.getBasedRevision(one)).toBe(3)
    expect(store.getBasedRevision(two)).toBe(8)
  })

  it('serializes only exact Conversation-bound terminal refs and referenced editors', () => {
    const root = useWorkspaceStore.getState().root
    if (root.type !== 'leaf') throw new Error('expected leaf')
    useWorkspaceStore.setState({
      root: {
        ...root,
        tabs: [
          { type: 'terminal', id: 'term-t-one', terminalId: 't-one' },
          { type: 'terminal', id: 'term-t-two', terminalId: 't-two' }
        ]
      }
    })
    useTerminalStore.setState({
      terminals: [
        { id: 't-one', projectId: 'same-project', shell: 'bash', name: 'one', conversationId: one },
        { id: 't-two', projectId: 'same-project', shell: 'bash', name: 'two', conversationId: two }
      ] as never
    })
    const value = buildSessionWorkspace(one)
    expect(value.resources).toEqual([
      { kind: 'terminal', terminalId: 't-one', conversationId: one }
    ])
    expect(value.topology).toMatchObject({ type: 'leaf', terminalIds: ['t-one'] })
    expect(JSON.stringify(value)).not.toMatch(/claim|envVars|credentials/i)
  })

  it('reads only the Conversation-backed ACP field and never treats an ACP SessionId as identity', () => {
    useAcpStore.setState({
      activeSessionId: one,
      sessions: {
        [one]: {
          id: one,
          agentId: 'agent-one',
          cwd: '/work',
          projectId: 'same-project',
          status: 'active',
          title: null,
          activeTurn: false,
          openTurnId: null,
          modes: null,
          configOptions: [],
          lastError: null,
          createdAt: 1
        }
      }
    })
    expect(getActiveConversationId()).toBeNull()
    useAcpStore.setState((state) => ({
      sessions: {
        ...state.sessions,
        [one]: { ...state.sessions[one], conversationId: two }
      }
    }))
    expect(getActiveConversationId()).toBe(two)
  })

  it('debounces writes and scopes stale conflicts to the target Conversation', async () => {
    vi.useFakeTimers()
    writeMock.mockResolvedValue({
      success: true,
      data: {
        status: 'conflict',
        currentRevision: 6,
        currentUpdatedAtUtc: '2026-08-15T10:00:00.000Z',
        currentUpdateIdentity: 'other'
      }
    })
    useSessionWorkspaceSyncStore.getState().setBasedRevision(one, 4)
    const { unmount } = renderHook(() => useSessionWorkspaceSync(one))
    act(() => useWorkspaceStore.setState({ activePaneId: 'changed' }))
    expect(writeMock).not.toHaveBeenCalled()
    await act(async () => vi.advanceTimersByTime(500))
    expect(writeMock).toHaveBeenCalledTimes(1)
    expect(useSessionWorkspaceSyncStore.getState().getConflict(one)).toMatchObject({
      conversationId: one,
      currentRevision: 6
    })
    expect(useSessionWorkspaceSyncStore.getState().getConflict(two)).toBeNull()
    unmount()
  })

  it('resolves reload and overwrite without changing another Conversation base', async () => {
    const store = useSessionWorkspaceSyncStore.getState()
    store.setBasedRevision(one, 2)
    store.setBasedRevision(two, 9)
    store.setConflict(one, {
      conversationId: one,
      currentRevision: 5,
      currentUpdatedAtUtc: '2026-08-15T10:00:00.000Z'
    })
    writeMock.mockResolvedValue({
      success: true,
      data: { status: 'updated', revision: 6, updatedAtUtc: '2026-08-15T10:00:01.000Z' }
    })
    await resolveSessionWorkspaceConflict(one, 'overwrite')
    expect(writeMock).toHaveBeenCalledWith(one, 5, expect.any(Object))
    expect(store.getBasedRevision(one)).toBe(6)
    expect(store.getBasedRevision(two)).toBe(9)
  })

  it('surfaces recovery-required and sends exact shared action CAS fields', async () => {
    const item = {
      recoveryId: 'a'.repeat(64),
      kind: 'ambiguous_workspace_manifest' as const,
      severity: 'warning' as const,
      sourcePaths: ['legacy/workspace.json'],
      conversationIds: [one, two],
      sourceSha256: ['e'.repeat(64)],
      candidateFacts: [],
      provenance: [],
      status: 'unresolved' as const,
      suggestedActions: [
        'inspect',
        'associateConversation',
        'startEmptyWorkspace',
        'dismissPreservedSource'
      ] as const,
      revision: 7,
      associationDecisions: []
    }
    getMock.mockResolvedValue({
      success: true,
      data: { status: 'recoveryRequired', conversationId: one, recoveryItems: [item] }
    })
    expect(await loadSessionWorkspace(one)).toBe(false)
    expect(useSessionWorkspaceSyncStore.getState().getRecoveryItems(one)).toEqual([item])
    recoveryMock.mockResolvedValue({
      success: true,
      data: {
        recoveryId: item.recoveryId,
        action: 'startEmptyWorkspace',
        authorization: 'mutation',
        status: 'resolvedStartedEmpty',
        recoveryRevision: 8,
        workspaceRevision: 1,
        workspaceChanged: true,
        sourcePaths: item.sourcePaths,
        sourceSha256: item.sourceSha256,
        candidateFacts: [],
        provenance: []
      }
    })
    getMock.mockResolvedValueOnce({
      success: true,
      data: { status: 'loaded', workspace: workspace(one, 1, 'empty') }
    })
    await resolveSessionWorkspaceRecovery(one, item, 'startEmptyWorkspace')
    expect(recoveryMock).toHaveBeenCalledWith(
      expect.objectContaining({
        recoveryId: item.recoveryId,
        expectedRevision: 7,
        action: 'startEmptyWorkspace',
        payload: { conversationId: one, expectedWorkspaceRevision: null }
      })
    )
  })

  it('returns recoveryRequired from a write without advancing revision', async () => {
    useSessionWorkspaceSyncStore.getState().setBasedRevision(one, 4)
    writeMock.mockResolvedValue({
      success: true,
      data: { status: 'recoveryRequired', recoveryItems: [] }
    })
    expect(await performSessionWorkspaceWrite(one)).toBe('recoveryRequired')
    expect(useSessionWorkspaceSyncStore.getState().getBasedRevision(one)).toBe(4)
  })
})
