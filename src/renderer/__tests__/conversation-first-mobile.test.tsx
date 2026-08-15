import type { RecoveryItemV1 } from '@shared/types/conversation-recovery.types'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useState } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ConversationRecoveryPanel } from '@/components/conversation/ConversationRecoveryPanel'
import { ExecutionTargetPicker } from '@/components/conversation/ExecutionTargetPicker'
import { useConversationStore } from '@/stores/conversation-store'
import { useTerminalStore } from '@/stores/terminal-store'

const ID = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'
const { mockConversationApi, mockTerminalApi } = vi.hoisted(() => ({
  mockConversationApi: {
    listConversations: vi.fn(),
    openConversation: vi.fn(),
    resolveRecovery: vi.fn()
  },
  mockTerminalApi: {
    closeView: vi.fn(),
    terminate: vi.fn()
  }
}))

vi.mock('@/lib/conversation-api', () => ({ conversationApi: mockConversationApi }))
vi.mock('@/lib/terminal-api', () => ({ terminalApi: mockTerminalApi }))
vi.mock('@/lib/log-api', () => ({ logFrontendError: vi.fn() }))
vi.mock('@/hooks/use-session-workspace-sync', () => ({ loadSessionWorkspace: vi.fn() }))

const recoveryItem: RecoveryItemV1 = {
  recoveryId: 'b'.repeat(64),
  kind: 'ambiguous_workspace_manifest',
  severity: 'warning',
  sourcePaths: ['legacy_workspace_manifests/0/phone.json'],
  conversationIds: [ID],
  sourceSha256: ['e'.repeat(64)],
  candidateFacts: [],
  provenance: [
    {
      sourceKind: 'legacy_workspace_manifests',
      relativePath: 'legacy_workspace_manifests/0/phone.json',
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
  revision: 3,
  associationDecisions: []
}

function PhoneHarness(): React.JSX.Element {
  const [drawerOpen, setDrawerOpen] = useState(false)
  const [target, setTarget] = useState({ kind: 'workspace' as const })
  const terminal = useTerminalStore((state) => state.terminals[0])
  const closeView = useTerminalStore((state) => state.closeTerminalView)
  const reopen = useTerminalStore((state) => state.reopenTerminalView)
  const terminate = useTerminalStore((state) => state.terminateTerminalResource)

  return (
    <main data-testid="phone-flow" style={{ width: 390 }}>
      <button
        type="button"
        aria-label="Open conversation drawer"
        onClick={() => setDrawerOpen(true)}
      >
        Conversations
      </button>
      {drawerOpen ? (
        <aside aria-label="Conversation drawer">
          <button type="button" aria-label="New chat">
            New chat
          </button>
          <button
            type="button"
            aria-label="Close conversation drawer"
            onClick={() => setDrawerOpen(false)}
          >
            Close
          </button>
        </aside>
      ) : null}
      <ExecutionTargetPicker
        projects={[]}
        value={target}
        attachment={null}
        workspaceCwd={`/visible/sessions/2026/08/15/${ID}`}
        onChange={setTarget}
        onAttachmentChange={() => undefined}
      />
      {terminal ? (
        <section aria-label="Terminal lifecycle">
          <output>{terminal.viewState ?? 'visible'}</output>
          <button type="button" onClick={() => void closeView(terminal.id)}>
            Close terminal view
          </button>
          <button type="button" onClick={() => reopen(terminal.id)}>
            Reopen terminal
          </button>
          <button type="button" onClick={() => void terminate(terminal.id)}>
            Terminate terminal
          </button>
        </section>
      ) : null}
    </main>
  )
}

describe('Conversation-first responsive phone matrix', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 390 })
    useConversationStore.getState().reset()
    useTerminalStore.setState({ terminals: [], activeTerminalId: '', ptyIdIndex: new Map() })
    const terminal = useTerminalStore
      .getState()
      .addTerminal('Phone shell', '', 'bash', `/visible/sessions/2026/08/15/${ID}`, [], ID)
    useTerminalStore.getState().setTerminalPtyId(terminal.id, 'pty-phone')
    mockTerminalApi.closeView.mockResolvedValue({ success: true, data: undefined })
    mockTerminalApi.terminate.mockResolvedValue({ success: true, data: undefined })
    mockConversationApi.openConversation.mockResolvedValue({
      success: true,
      data: {
        conversation: {
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
        },
        workspace: { status: 'missing', conversationId: ID }
      }
    })
  })

  it('opens the mobile drawer and keeps New Chat enabled without a project', () => {
    render(<PhoneHarness />)
    expect(screen.queryByLabelText('Conversation drawer')).not.toBeInTheDocument()
    fireEvent.click(screen.getByLabelText('Open conversation drawer'))
    expect(screen.getByLabelText('Conversation drawer')).toBeVisible()
    expect(screen.getByLabelText('New chat')).toBeEnabled()
    expect(screen.getByLabelText('Execution target')).toHaveTextContent('Conversation workspace')
  })

  it('keeps terminal close-view, reopen, and explicit terminate distinct at phone width', async () => {
    render(<PhoneHarness />)
    fireEvent.click(screen.getByRole('button', { name: 'Close terminal view' }))
    await waitFor(() => expect(mockTerminalApi.closeView).toHaveBeenCalledWith('pty-phone'))
    expect(mockTerminalApi.terminate).not.toHaveBeenCalled()
    expect(screen.getByText('hidden')).toBeVisible()

    fireEvent.click(screen.getByRole('button', { name: 'Reopen terminal' }))
    expect(screen.getByText('visible')).toBeVisible()
    fireEvent.click(screen.getByRole('button', { name: 'Terminate terminal' }))
    await waitFor(() => expect(mockTerminalApi.terminate).toHaveBeenCalledWith('pty-phone'))
  })

  it('reopens the same canonical Conversation after background/reconnect without terminating PTY', async () => {
    document.dispatchEvent(new Event('visibilitychange'))
    await useConversationStore.getState().openConversation(ID)
    window.dispatchEvent(new Event('online'))
    await useConversationStore.getState().openConversation(ID)
    expect(mockConversationApi.openConversation).toHaveBeenCalledTimes(2)
    expect(useConversationStore.getState().activeConversationId).toBe(ID)
    expect(mockTerminalApi.terminate).not.toHaveBeenCalled()
    expect(useTerminalStore.getState().terminals[0].ptyId).toBe('pty-phone')
  })

  it('shows all recovery actions and immutable evidence at 390px', async () => {
    mockConversationApi.resolveRecovery.mockResolvedValue({
      success: true,
      data: {
        recoveryId: recoveryItem.recoveryId,
        action: 'startEmptyWorkspace',
        authorization: 'mutation',
        status: 'resolvedStartedEmpty',
        recoveryRevision: 4,
        workspaceRevision: 1,
        workspaceChanged: true,
        sourcePaths: recoveryItem.sourcePaths,
        sourceSha256: recoveryItem.sourceSha256,
        candidateFacts: recoveryItem.candidateFacts,
        provenance: recoveryItem.provenance
      }
    })
    render(<ConversationRecoveryPanel items={[recoveryItem]} conversationId={ID} embedded />)
    for (const action of recoveryItem.suggestedActions) {
      expect(document.querySelector(`[data-recovery-action="${action}"]`)).toBeVisible()
    }
    expect(screen.getAllByText(/legacy_workspace_manifests\/0\/phone.json/).length).toBeGreaterThan(
      0
    )
    fireEvent.click(screen.getByRole('button', { name: 'Start empty workspace' }))
    await waitFor(() => expect(mockConversationApi.resolveRecovery).toHaveBeenCalledTimes(1))
    expect(mockConversationApi.resolveRecovery.mock.calls[0][0]).toMatchObject({
      action: 'startEmptyWorkspace',
      expectedRevision: 3,
      payload: { conversationId: ID, expectedWorkspaceRevision: null }
    })
  })
})
