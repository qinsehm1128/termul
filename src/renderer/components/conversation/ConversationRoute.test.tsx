import type { ConversationRecordV2 } from '@shared/types/conversation.types'
import { render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ChatRoute } from '@/components/ChatRoute'
import { conversationApi } from '@/lib/conversation-api'
import { useAcpStore } from '@/stores/acp-store'
import { useConversationStore } from '@/stores/conversation-store'
import { useWorkspaceStore } from '@/stores/workspace-store'
import { ConversationRoute } from './ConversationRoute'

const { mockLoadSessionWorkspace, mockAddAgentChatTab, mockOpenHistorySession } = vi.hoisted(
  () => ({
    mockLoadSessionWorkspace: vi.fn(),
    mockAddAgentChatTab: vi.fn(),
    mockOpenHistorySession: vi.fn()
  })
)

vi.mock('@/lib/conversation-api', () => ({
  conversationApi: {
    openConversation: vi.fn(),
    resolveLegacyConversationId: vi.fn()
  }
}))

vi.mock('@/hooks/use-session-workspace-sync', () => ({
  loadSessionWorkspace: mockLoadSessionWorkspace
}))

vi.mock('@/lib/log-api', () => ({ logFrontendError: vi.fn() }))

const conversationId = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'

const conversation: ConversationRecordV2 = {
  schemaVersion: 2,
  conversationId,
  createdAtUtc: '2026-08-15T09:45:15.123Z',
  creationPartition: { year: 2026, month: 8, day: 15, path: '2026/08/15' },
  workspaceCwd: '/workspace/conversation',
  executionTarget: { kind: 'workspace' },
  projectAttachment: null,
  lifecycleState: 'ready',
  lastSeq: 2,
  createdBy: 'termul'
}

beforeEach(() => {
  vi.clearAllMocks()
  useConversationStore.getState().reset()
  mockLoadSessionWorkspace.mockResolvedValue(true)
  mockOpenHistorySession.mockResolvedValue(undefined)
  useAcpStore.setState({
    sessions: {},
    sessionIndex: [
      {
        id: 'opaque-agent-session',
        conversationId,
        agentId: 'agent-1',
        title: 'Canonical chat',
        cwd: '/workspace/conversation',
        projectId: '',
        createdAt: 1,
        lastActivityAt: 2,
        messageCount: 0,
        status: 'closed'
      }
    ],
    openHistorySession: mockOpenHistorySession
  })
  useWorkspaceStore.setState({ addAgentChatTab: mockAddAgentChatTab })
})

function renderCanonical(id = conversationId): void {
  render(
    <MemoryRouter initialEntries={[`/c/${id}`]}>
      <Routes>
        <Route path="/c/:conversationId" element={<ConversationRoute />} />
      </Routes>
    </MemoryRouter>
  )
}

describe('ConversationRoute canonical open', () => {
  it('opens by ConversationId, restores workspace, and uses the opaque binding only internally', async () => {
    vi.mocked(conversationApi.openConversation).mockResolvedValue({
      success: true,
      data: {
        conversation,
        workspace: { status: 'missing', conversationId }
      }
    })

    renderCanonical()

    await waitFor(() => {
      expect(conversationApi.openConversation).toHaveBeenCalledWith(conversationId)
      expect(mockLoadSessionWorkspace).toHaveBeenCalledWith(conversationId)
      expect(mockOpenHistorySession).toHaveBeenCalledWith('opaque-agent-session')
      expect(mockAddAgentChatTab).toHaveBeenCalledWith('opaque-agent-session', undefined, false)
    })
    expect(useConversationStore.getState().activeConversationId).toBe(conversationId)
  })

  it('renders a stable not-found error and never stores the route value as an ACP id', async () => {
    vi.mocked(conversationApi.openConversation).mockResolvedValue({
      success: false,
      code: 'CONVERSATION_NOT_FOUND',
      error: 'missing'
    })

    renderCanonical()

    expect(await screen.findByRole('alert')).toHaveAttribute(
      'data-error-code',
      'CONVERSATION_NOT_FOUND'
    )
    expect(mockOpenHistorySession).not.toHaveBeenCalled()
  })

  it('renders recovery-required with an explicit retry action', async () => {
    vi.mocked(conversationApi.openConversation).mockResolvedValue({
      success: false,
      code: 'CONVERSATION_RECOVERY_REQUIRED',
      error: 'recover'
    })

    renderCanonical()

    expect(await screen.findByRole('alert')).toHaveAttribute(
      'data-error-code',
      'CONVERSATION_RECOVERY_REQUIRED'
    )
    expect(screen.getByRole('button', { name: 'Try Again' })).toBeInTheDocument()
  })

  it('rejects a non-canonical id before calling the facade', async () => {
    renderCanonical('opaque-agent-session')

    expect(await screen.findByRole('alert')).toHaveAttribute(
      'data-error-code',
      'CONVERSATION_INVALID_ID'
    )
    expect(conversationApi.openConversation).not.toHaveBeenCalled()
  })
})

describe('ChatRoute legacy redirect', () => {
  it('uses the typed read-only resolver and replace-redirects to the canonical route', async () => {
    vi.mocked(conversationApi.resolveLegacyConversationId).mockResolvedValue({
      success: true,
      data: { conversationId, canonicalRoute: `#/c/${conversationId}` }
    })

    render(
      <MemoryRouter initialEntries={['/legacy/history/opaque-history']}>
        <Routes>
          <Route
            path="/legacy/history/:legacyValue"
            element={<ChatRoute sourceKind="legacyChatHistoryId" />}
          />
          <Route path="/c/:conversationId" element={<div>canonical destination</div>} />
        </Routes>
      </MemoryRouter>
    )

    expect(await screen.findByText('canonical destination')).toBeInTheDocument()
    expect(conversationApi.resolveLegacyConversationId).toHaveBeenCalledWith({
      sourceKind: 'legacyChatHistoryId',
      value: 'opaque-history'
    })
  })

  it('resolves a UUID-shaped legacy value instead of treating it as a canonical route key', async () => {
    vi.mocked(conversationApi.resolveLegacyConversationId).mockResolvedValue({
      success: true,
      data: { conversationId, canonicalRoute: `#/c/${conversationId}` }
    })

    render(
      <MemoryRouter initialEntries={[`/legacy/session/${conversationId}`]}>
        <Routes>
          <Route
            path="/legacy/session/:legacyValue"
            element={<ChatRoute sourceKind="legacyAgentSessionId" />}
          />
          <Route path="/c/:conversationId" element={<div>canonical destination</div>} />
        </Routes>
      </MemoryRouter>
    )

    expect(await screen.findByText('canonical destination')).toBeInTheDocument()
    expect(conversationApi.resolveLegacyConversationId).toHaveBeenCalledWith({
      sourceKind: 'legacyAgentSessionId',
      value: conversationId
    })
    expect(useConversationStore.getState().activeConversationId).toBeNull()
  })

  it.each([
    'CONVERSATION_NOT_FOUND',
    'LEGACY_ID_AMBIGUOUS'
  ])('renders stable accessible %s resolver errors', async (code) => {
    vi.mocked(conversationApi.resolveLegacyConversationId).mockResolvedValue({
      success: false,
      code,
      error: code
    })

    render(
      <MemoryRouter initialEntries={['/legacy/storage/legacy-key']}>
        <Routes>
          <Route
            path="/legacy/storage/:legacyValue"
            element={<ChatRoute sourceKind="legacyStorageKey" />}
          />
        </Routes>
      </MemoryRouter>
    )

    expect(await screen.findByRole('alert')).toHaveAttribute('data-error-code', code)
    expect(useConversationStore.getState().summariesById['legacy-key']).toBeUndefined()
  })
})
