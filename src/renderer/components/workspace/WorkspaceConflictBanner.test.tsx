import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const { conflictMock, recoveryMock } = vi.hoisted(() => ({
  conflictMock: vi.fn(),
  recoveryMock: vi.fn()
}))

vi.mock('@/hooks/use-session-workspace-sync', () => ({
  resolveSessionWorkspaceConflict: conflictMock,
  resolveSessionWorkspaceRecovery: recoveryMock
}))

import { useSessionWorkspaceSyncStore } from '@/stores/session-workspace-sync-store'
import { WorkspaceConflictBanner } from './WorkspaceConflictBanner'

const one = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'
const two = '5f7a1c01-4d1b-4c8a-af01-0123456789ab'
const recoveryItem = {
  recoveryId: 'a'.repeat(64),
  kind: 'ambiguous_workspace_manifest' as const,
  severity: 'warning' as const,
  sourcePaths: ['legacy_workspace_manifests/0/shared.json'],
  conversationIds: [one, two],
  sourceSha256: ['e'.repeat(64)],
  candidateFacts: [],
  provenance: [
    {
      sourceKind: 'legacy_workspace_manifests',
      relativePath: 'legacy_workspace_manifests/0/shared.json',
      sha256: 'e'.repeat(64),
      preservedReadOnly: true as const
    }
  ],
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

beforeEach(() => {
  vi.clearAllMocks()
  useSessionWorkspaceSyncStore.setState({
    activeConversationId: one,
    basedRevisionByConversation: {},
    conflictsByConversation: {},
    recoveryByConversation: {},
    loadOutcomeByConversation: {},
    restoreInProgressByConversation: {}
  })
})

afterEach(cleanup)

describe('WorkspaceConflictBanner', () => {
  it('scopes a stale conflict to the active Conversation, not the project store', () => {
    useSessionWorkspaceSyncStore.getState().setConflict(one, {
      conversationId: one,
      currentRevision: 5,
      currentUpdatedAtUtc: '2026-08-15T10:00:00.000Z',
      currentUpdateIdentity: 'other-client'
    })
    const { rerender } = render(<WorkspaceConflictBanner />)
    expect(screen.getByRole('alert')).toHaveAttribute('data-conversation-id', one)
    expect(screen.getByText(/revision 5/)).toBeInTheDocument()

    act(() => useSessionWorkspaceSyncStore.getState().setActiveConversationId(two))
    rerender(<WorkspaceConflictBanner />)
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('calls Conversation-scoped conflict actions', () => {
    useSessionWorkspaceSyncStore.getState().setConflict(one, {
      conversationId: one,
      currentRevision: 5,
      currentUpdatedAtUtc: '2026-08-15T10:00:00.000Z'
    })
    render(<WorkspaceConflictBanner conversationId={one} />)
    fireEvent.click(screen.getByRole('button', { name: 'Reload from host' }))
    fireEvent.click(screen.getByRole('button', { name: 'Overwrite with local' }))
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }))
    expect(conflictMock).toHaveBeenNthCalledWith(1, one, 'reload')
    expect(conflictMock).toHaveBeenNthCalledWith(2, one, 'overwrite')
    expect(conflictMock).toHaveBeenNthCalledWith(3, one, 'dismiss')
  })

  it('renders immutable source/checksum context and the exact shared recovery actions', () => {
    useSessionWorkspaceSyncStore.getState().setRecoveryItems(one, [recoveryItem])
    render(<WorkspaceConflictBanner conversationId={one} />)
    expect(screen.getByText('ambiguous_workspace_manifest')).toBeInTheDocument()
    expect(screen.getByText(/legacy_workspace_manifests\/0\/shared.json/)).toBeInTheDocument()
    expect(screen.getByText(new RegExp(`sha256:${'e'.repeat(64)}`))).toBeInTheDocument()
    for (const action of recoveryItem.suggestedActions) {
      expect(screen.getByRole('button', { name: action })).toBeInTheDocument()
    }
  })

  it('invokes every exact recovery action with the active Conversation and RecoveryItem revision', () => {
    useSessionWorkspaceSyncStore.getState().setRecoveryItems(one, [recoveryItem])
    render(<WorkspaceConflictBanner conversationId={one} />)
    for (const action of recoveryItem.suggestedActions) {
      fireEvent.click(screen.getByRole('button', { name: action }))
    }
    expect(recoveryMock.mock.calls).toEqual(
      recoveryItem.suggestedActions.map((action) => [one, recoveryItem, action])
    )
  })

  it('uses accessible buttons and responsive wrapping', () => {
    useSessionWorkspaceSyncStore.getState().setRecoveryItems(one, [recoveryItem])
    render(<WorkspaceConflictBanner conversationId={one} />)
    expect(screen.getByRole('alert')).toHaveAttribute('aria-live', 'polite')
    for (const button of screen.getAllByRole('button')) {
      expect(button).toHaveAttribute('type', 'button')
      expect(button.parentElement?.className).toContain('flex-wrap')
    }
  })
})
