import type { ConversationRecordV2 } from '@shared/types/conversation.types'
import type { RecoveryItemV1 } from '@shared/types/conversation-recovery.types'
import type { SessionWorkspaceV1 } from '@shared/types/session-workspace.types'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useState } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ConversationRecoveryPanel } from '@/components/conversation/ConversationRecoveryPanel'
import { ExecutionTargetPicker } from '@/components/conversation/ExecutionTargetPicker'
import { PaneContent } from '@/components/workspace/PaneContent'
import { loadSessionWorkspace } from '@/hooks/use-session-workspace-sync'
import { useConversationStore } from '@/stores/conversation-store'
import { useProjectStore } from '@/stores/project-store'
import { useSessionWorkspaceSyncStore } from '@/stores/session-workspace-sync-store'
import { useTerminalStore } from '@/stores/terminal-store'
import { useWorkspaceStore } from '@/stores/workspace-store'

const ID = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'
const { mockConversationApi, mockTerminalApi, mockSessionWorkspaceApi } = vi.hoisted(() => ({
  mockConversationApi: {
    listConversations: vi.fn(),
    openConversation: vi.fn(),
    resolveRecovery: vi.fn()
  },
  mockTerminalApi: {
    resume: vi.fn(),
    spawn: vi.fn(),
    closeView: vi.fn(),
    terminate: vi.fn()
  },
  mockSessionWorkspaceApi: {
    getWorkspace: vi.fn(),
    writeWorkspace: vi.fn(),
    resolveRecovery: vi.fn()
  }
}))

vi.mock('@/lib/conversation-api', () => ({ conversationApi: mockConversationApi }))
vi.mock('@/lib/terminal-api', () => ({ terminalApi: mockTerminalApi }))
vi.mock('@/lib/session-workspace-api', () => ({ sessionWorkspaceApi: mockSessionWorkspaceApi }))
vi.mock('@/lib/log-api', () => ({ logFrontendError: vi.fn() }))
vi.mock('@/components/terminal/ConnectedTerminal', () => ({
  ConnectedTerminal: ({ terminalId }: { terminalId?: string }) => (
    <div data-testid="connected-terminal">connected:{terminalId}</div>
  )
}))

const conversation: ConversationRecordV2 = {
  schemaVersion: 2,
  conversationId: ID,
  createdAtUtc: '2026-08-15T09:45:15.123Z',
  creationPartition: { year: 2026, month: 8, day: 15, path: '2026/08/15' },
  workspaceCwd: `/visible/sessions/2026/08/15/${ID}`,
  executionTarget: { kind: 'workspace' },
  projectAttachment: null,
  lifecycleState: 'ready',
  lastSeq: 0,
  createdBy: 'termul'
}

function coldTerminalWorkspace(): SessionWorkspaceV1 {
  return {
    schemaVersion: 1,
    conversationId: ID,
    revision: 9,
    updatedAtUtc: '2026-08-15T10:00:00.000Z',
    topology: {
      type: 'leaf',
      id: 'cold-terminal-pane',
      terminalIds: ['cold-terminal-record'],
      editorIds: [],
      activeTabId: 'term-cold-terminal-record'
    },
    activePaneId: 'cold-terminal-pane',
    resources: [
      {
        kind: 'terminal',
        terminalId: 'pty-cold-live',
        terminalRecordId: 'cold-terminal-record',
        conversationId: ID
      }
    ],
    projectionState: { status: 'native' }
  }
}

const recoveryItem: RecoveryItemV1 = {
  recoveryId: 'a'.repeat(64),
  kind: 'ambiguous_workspace_manifest',
  severity: 'warning',
  sourcePaths: ['legacy_workspace_manifests/0/shared.json'],
  conversationIds: [ID],
  sourceSha256: ['e'.repeat(64)],
  candidateFacts: [{ candidate: 'preserved' }],
  provenance: [
    {
      sourceKind: 'legacy_workspace_manifests',
      relativePath: 'legacy_workspace_manifests/0/shared.json',
      sha256: 'e'.repeat(64),
      preservedReadOnly: true
    }
  ],
  status: 'unresolved',
  suggestedActions: [
    'inspect',
    'associateConversation',
    'startEmptyWorkspace',
    'dismissPreservedSource'
  ],
  revision: 1,
  associationDecisions: []
}

function ConflictView(): React.JSX.Element {
  const conflict = useSessionWorkspaceSyncStore((state) => state.getConflict(ID))
  return conflict ? (
    <div role="alert">
      Workspace conflict at revision {conflict.currentRevision}; canonical Conversation remains {ID}
    </div>
  ) : (
    <div role="status">Workspace ready</div>
  )
}

function TargetHarness(): React.JSX.Element {
  const [target, setTarget] = useState(conversation.executionTarget)
  const [attachment, setAttachment] = useState(conversation.projectAttachment)
  return (
    <ExecutionTargetPicker
      projects={[]}
      value={target}
      attachment={attachment}
      conversation={conversation}
      workspaceCwd={conversation.workspaceCwd}
      onChange={setTarget}
      onAttachmentChange={setAttachment}
    />
  )
}

describe('Conversation-first desktop/browser flow matrix', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    useConversationStore.getState().reset()
    useSessionWorkspaceSyncStore.setState({
      activeConversationId: null,
      basedRevisionByConversation: {},
      conflictsByConversation: {},
      recoveryByConversation: {},
      loadOutcomeByConversation: {},
      restoreInProgressByConversation: {}
    })
    useTerminalStore.setState({ terminals: [], activeTerminalId: '', ptyIdIndex: new Map() })
    useWorkspaceStore.getState().resetLayout()
    useProjectStore.setState({ activeProjectId: '' })
    mockTerminalApi.resume.mockResolvedValue({
      success: false,
      error: 'Unauthorized',
      code: 'UNAUTHORIZED'
    })
    mockTerminalApi.spawn.mockResolvedValue({
      success: false,
      error: 'not expected',
      code: 'SPAWN_FAILED'
    })
    mockTerminalApi.closeView.mockResolvedValue({ success: true, data: undefined })
    mockTerminalApi.terminate.mockResolvedValue({ success: true, data: undefined })
    mockSessionWorkspaceApi.getWorkspace.mockResolvedValue({
      success: true,
      data: { status: 'missing', conversationId: ID }
    })
  })

  it('supports zero-project New Chat target selection without changing canonical identity', () => {
    render(<TargetHarness />)
    expect(screen.getByLabelText('Execution target')).toHaveTextContent('Conversation workspace')
    expect(screen.getByTestId('workspace-identity-unchanged')).toHaveAttribute(
      'data-unchanged',
      'true'
    )
    expect(screen.getByText(ID)).toBeVisible()
    expect(screen.getByText('2026/08/15')).toBeVisible()
    expect(screen.getAllByText(conversation.workspaceCwd).length).toBeGreaterThan(0)
    expect(screen.getByRole('button', { name: 'Attach project context' })).toBeDisabled()
  })

  it('opens and reopens the canonical Conversation route by ConversationId', async () => {
    mockConversationApi.openConversation.mockResolvedValue({
      success: true,
      data: { conversation, workspace: { status: 'missing', conversationId: ID } }
    })
    const opened = await useConversationStore.getState().openConversation(ID)
    expect(opened?.conversation.conversationId).toBe(ID)
    expect(useConversationStore.getState().activeConversationId).toBe(ID)
    useConversationStore.getState().setActiveConversationId(null)
    await useConversationStore.getState().openConversation(ID)
    expect(useConversationStore.getState().activeConversationId).toBe(ID)
    expect(mockConversationApi.openConversation).toHaveBeenCalledTimes(2)
  })

  it('cold-loads a persisted terminal tab and replay without spawning a replacement', async () => {
    const persisted = coldTerminalWorkspace()
    useConversationStore.getState().replaceSummaries([conversation])
    useConversationStore.getState().setActiveConversationId(ID)
    mockSessionWorkspaceApi.getWorkspace.mockResolvedValue({
      success: true,
      data: { status: 'loaded', workspace: persisted }
    })
    mockTerminalApi.resume.mockImplementation(async (request) => {
      const hydrated = useTerminalStore.getState().findTerminalByPtyId(request.terminalId)
      expect(hydrated).toMatchObject({
        id: 'cold-terminal-record',
        healthStatus: 'disconnected',
        conversationId: ID
      })
      useTerminalStore.getState().appendTranscript(request.terminalId, 'replayed cold output')
      return {
        success: true,
        data: {
          terminal: {
            id: request.terminalId,
            shell: 'bash',
            cwd: conversation.workspaceCwd,
            pid: 73,
            cols: 100,
            rows: 30,
            latestSeq: 21,
            gap: false
          },
          claim: 'renderer-memory-only'
        }
      }
    })

    await expect(loadSessionWorkspace(ID)).resolves.toBe(true)

    const root = useWorkspaceStore.getState().root
    if (root.type !== 'leaf') throw new Error('expected restored leaf')
    render(<PaneContent pane={root} />)
    expect(await screen.findByTestId('connected-terminal')).toHaveTextContent(
      'connected:pty-cold-live'
    )
    expect(root.tabs).toEqual([
      {
        type: 'terminal',
        id: 'term-cold-terminal-record',
        terminalId: 'cold-terminal-record'
      }
    ])
    expect(useTerminalStore.getState().peekTranscript('pty-cold-live')).toBe('replayed cold output')
    expect(useTerminalStore.getState().terminals[0]).toMatchObject({
      id: 'cold-terminal-record',
      ptyId: 'pty-cold-live',
      healthStatus: 'running',
      resumeCursor: 21,
      claim: 'renderer-memory-only'
    })
    expect(mockTerminalApi.resume).toHaveBeenCalledWith({
      conversationId: ID,
      terminalId: 'pty-cold-live',
      lastSeq: 0
    })
    expect(mockTerminalApi.spawn).not.toHaveBeenCalled()
    expect(mockTerminalApi.terminate).not.toHaveBeenCalled()
    expect(JSON.stringify(persisted)).not.toMatch(/claim|token|terminalOutput/i)
  })

  it('surfaces independent workspace conflicts without replacing Conversation identity', () => {
    render(<ConflictView />)
    act(() => {
      useSessionWorkspaceSyncStore.getState().setActiveConversationId(ID)
      useSessionWorkspaceSyncStore.getState().setConflict(ID, {
        conversationId: ID,
        currentRevision: 7,
        currentUpdatedAtUtc: '2026-08-15T10:00:00.000Z',
        currentUpdateIdentity: 'browser-b'
      })
    })
    expect(screen.getByRole('alert')).toHaveTextContent('revision 7')
    expect(screen.getByRole('alert')).toHaveTextContent(ID)
    expect(useSessionWorkspaceSyncStore.getState().activeConversationId).toBe(ID)
  })

  it('executes exact recovery actions and preserves immutable source evidence', async () => {
    mockConversationApi.resolveRecovery.mockImplementation(async (request) => ({
      success: true,
      data: {
        recoveryId: request.recoveryId,
        action: request.action,
        authorization: request.action === 'inspect' ? 'read' : 'mutation',
        status: request.action === 'inspect' ? 'unresolved' : 'resolvedAssociated',
        recoveryRevision: request.action === 'inspect' ? 1 : 2,
        workspaceRevision: null,
        workspaceChanged: false,
        sourcePaths: ['transport-must-not-replace-source'],
        sourceSha256: ['f'.repeat(64)],
        candidateFacts: [],
        provenance: []
      }
    }))
    render(<ConversationRecoveryPanel items={[recoveryItem]} conversationId={ID} embedded />)

    for (const action of recoveryItem.suggestedActions) {
      expect(document.querySelector(`[data-recovery-action="${action}"]`)).toBeVisible()
    }
    expect(
      screen.getAllByText(/legacy_workspace_manifests\/0\/shared.json/).length
    ).toBeGreaterThan(0)
    expect(screen.getAllByText(new RegExp(`sha256:${'e'.repeat(64)}`)).length).toBeGreaterThan(0)

    fireEvent.click(screen.getByRole('button', { name: 'Inspect preserved source' }))
    await waitFor(() => expect(mockConversationApi.resolveRecovery).toHaveBeenCalledTimes(1))
    expect(mockConversationApi.resolveRecovery.mock.calls[0][0]).toEqual({
      recoveryId: recoveryItem.recoveryId,
      expectedRevision: 1,
      action: 'inspect',
      payload: {}
    })
    expect(
      screen.getAllByText(/legacy_workspace_manifests\/0\/shared.json/).length
    ).toBeGreaterThan(0)
    expect(screen.queryByText('transport-must-not-replace-source')).not.toBeInTheDocument()
  })

  it.each([
    'FORBIDDEN',
    'UNAUTHORIZED',
    'CONVERSATION_RECOVERY_REQUIRED'
  ])('shows stable adapter error %s through an accessible alert', async (code) => {
    mockConversationApi.resolveRecovery.mockResolvedValue({
      success: false,
      code,
      error: code
    })
    render(<ConversationRecoveryPanel items={[recoveryItem]} conversationId={ID} embedded />)
    fireEvent.click(screen.getByRole('button', { name: 'Inspect preserved source' }))
    const alert = await screen.findByRole('alert')
    expect(alert).toHaveAttribute('data-error-code', code)
  })

  it('keeps Chat state intact while terminal close-view/reopen/terminate stay distinct', async () => {
    const terminal = useTerminalStore
      .getState()
      .addTerminal('Conversation shell', '', 'bash', conversation.workspaceCwd, [], ID)
    useTerminalStore.getState().setTerminalPtyId(terminal.id, 'pty-live')
    useConversationStore.getState().replaceSummaries([conversation])
    useConversationStore.getState().setActiveConversationId(ID)

    await expect(useTerminalStore.getState().closeTerminalView(terminal.id)).resolves.toBe(true)
    expect(mockTerminalApi.closeView).toHaveBeenCalledWith('pty-live')
    expect(mockTerminalApi.terminate).not.toHaveBeenCalled()
    expect(useTerminalStore.getState().terminals[0].viewState).toBe('hidden')
    expect(useConversationStore.getState().summariesById[ID]).toEqual(conversation)

    useTerminalStore.getState().reopenTerminalView(terminal.id)
    expect(useTerminalStore.getState().terminals[0].viewState).toBe('visible')
    await expect(useTerminalStore.getState().terminateTerminalResource(terminal.id)).resolves.toBe(
      true
    )
    expect(mockTerminalApi.terminate).toHaveBeenCalledWith('pty-live')
    expect(useConversationStore.getState().summariesById[ID]).toEqual(conversation)
  })
})
