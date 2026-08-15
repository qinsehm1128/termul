import { render, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { CONTEXT_BAR_SETTINGS_KEY } from '@/types/settings'
import TauriApp from './TauriApp'

const {
  mockPersistenceRead,
  mockSessionWorkspaceBootstrap,
  mockConversationHostBootstrap,
  mockConversationLifecycle,
  mockTerminalResourceLifecycle
} = vi.hoisted(() => ({
  mockPersistenceRead: vi.fn(),
  mockSessionWorkspaceBootstrap: vi.fn(),
  mockConversationHostBootstrap: vi.fn(),
  mockConversationLifecycle: vi.fn(),
  mockTerminalResourceLifecycle: vi.fn()
}))

vi.mock('@/lib/api', () => ({
  persistenceApi: {
    read: mockPersistenceRead
  },
  terminalApi: {
    onData: vi.fn(() => vi.fn())
  },
  sessionApi: {
    hasSession: vi.fn(async () => ({ success: true, data: false })),
    restore: vi.fn(async () => ({
      success: false,
      error: 'No session',
      code: 'SESSION_NOT_FOUND'
    })),
    save: vi.fn(),
    clear: vi.fn(),
    flush: vi.fn()
  }
}))

vi.mock('@/hooks/use-session-workspace-sync', () => ({
  useSessionWorkspaceBootstrap: mockSessionWorkspaceBootstrap
}))

vi.mock('./hooks/use-conversation-host-bootstrap', () => ({
  useConversationHostBootstrap: mockConversationHostBootstrap
}))

vi.mock('@/components/conversation/ConversationHostStatus', () => ({
  ConversationHostStatus: () => <div data-testid="conversation-host-status" />
}))

vi.mock('@/components/conversation/ConversationRecoveryPanel', () => ({
  ConversationRecoveryPanel: () => <div data-testid="conversation-recovery-panel" />
}))

vi.mock('./hooks/use-conversation-lifecycle', () => ({
  useConversationLifecycle: mockConversationLifecycle
}))

vi.mock('./hooks/use-terminal-resource-lifecycle', () => ({
  useTerminalResourceLifecycle: mockTerminalResourceLifecycle
}))

vi.mock('@/hooks/use-window-state', () => ({
  useWindowState: () => false
}))

vi.mock('./layouts/WorkspaceLayout', async () => {
  const { Outlet } = await import('react-router-dom')
  return { default: () => <Outlet /> }
})

vi.mock('@/components/conversation/ConversationRoute', () => ({
  ConversationRoute: () => <div data-testid="canonical-conversation-route" />
}))

vi.mock('@/components/ChatRoute', () => ({
  ChatRoute: ({ sourceKind }: { sourceKind: string }) => (
    <div data-testid="legacy-conversation-route" data-source-kind={sourceKind} />
  )
}))

vi.mock('./pages/WorkspaceDashboard', () => ({
  default: () => null
}))

vi.mock('./pages/ProjectSettings', () => ({
  default: () => null
}))

vi.mock('./pages/AppPreferences', () => ({
  default: () => null
}))

vi.mock('./pages/WorkspaceSnapshots', () => ({
  default: () => null
}))

vi.mock('./pages/NotFound', () => ({
  default: () => null
}))

vi.mock('./hooks/useTerminalAutoSave', () => ({
  useTerminalAutoSave: () => undefined
}))

vi.mock('./hooks/use-terminal-restore', () => ({
  useTerminalRestore: () => undefined
}))

vi.mock('./hooks/use-terminal-detached-output', () => ({
  useTerminalDetachedOutput: () => undefined
}))

vi.mock('./hooks/use-cwd', () => ({
  useCwd: () => undefined
}))

vi.mock('./hooks/use-git-branch', () => ({
  useGitBranch: () => undefined
}))

vi.mock('./hooks/use-git-status', () => ({
  useGitStatus: () => undefined
}))

vi.mock('./hooks/use-exit-code', () => ({
  useExitCode: () => undefined
}))

vi.mock('./hooks/use-app-settings', () => ({
  useAppSettingsLoader: () => undefined
}))

vi.mock('./hooks/use-keyboard-shortcuts', () => ({
  useKeyboardShortcutsLoader: () => undefined
}))

vi.mock('./hooks/use-projects-persistence', () => ({
  useProjectsLoader: () => undefined,
  useProjectsAutoSave: () => undefined
}))

vi.mock('./hooks/use-menu-updater-listener', () => ({
  useMenuUpdaterListener: () => undefined
}))

vi.mock('./hooks/use-updater', () => ({
  useUpdateCheck: () => undefined
}))

vi.mock('./components/UpdateAvailableToast', () => ({
  useUpdateToast: () => undefined
}))

const { mockUseVisibilityState } = vi.hoisted(() => ({
  mockUseVisibilityState: vi.fn(() => undefined)
}))

vi.mock('./hooks/use-visibility-state', () => ({
  useVisibilityState: mockUseVisibilityState
}))

vi.mock('./hooks/use-terminal-exit-notification', () => ({
  useTerminalExitNotification: () => undefined
}))

vi.mock('@/lib/tauri-notification-api', () => ({
  initNotificationPermissions: () => Promise.resolve(),
  sendDesktopNotification: () => Promise.resolve()
}))

beforeEach(() => {
  vi.clearAllMocks()
  window.location.hash = '#/'
  mockPersistenceRead.mockResolvedValue({
    success: false,
    error: 'Key not found',
    code: 'KEY_NOT_FOUND'
  })
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('TauriApp', () => {
  it('loads context bar settings on mount', async () => {
    render(<TauriApp />)

    await waitFor(() => {
      expect(mockPersistenceRead).toHaveBeenCalledWith(CONTEXT_BAR_SETTINGS_KEY)
    })
  })

  it('wires app visibility tracking at app scope', () => {
    render(<TauriApp />)
    expect(mockUseVisibilityState).toHaveBeenCalledTimes(1)
  })

  it('mounts the portable SessionWorkspace bootstrap at the desktop root', () => {
    render(<TauriApp />)
    expect(mockSessionWorkspaceBootstrap).toHaveBeenCalledTimes(1)
  })

  it('mounts shared Conversation creation and recovery wiring at the desktop root', () => {
    render(<TauriApp />)
    expect(mockConversationHostBootstrap).toHaveBeenCalledTimes(1)
    expect(document.querySelector('[data-testid="conversation-host-status"]')).not.toBeNull()
    expect(document.querySelector('[data-testid="conversation-recovery-panel"]')).not.toBeNull()
  })

  it('mounts Conversation lifecycle reconciliation at the desktop root', () => {
    render(<TauriApp />)
    expect(mockConversationLifecycle).toHaveBeenCalledTimes(1)
  })

  it('mounts terminal resource reconciliation at the desktop root', () => {
    render(<TauriApp />)
    expect(mockTerminalResourceLifecycle).toHaveBeenCalledTimes(1)
  })

  it('registers the canonical Conversation route in the desktop root', async () => {
    window.location.hash = '#/c/018f7a1c-1b4d-7c8a-9f01-0123456789ab'
    render(<TauriApp />)

    await waitFor(() => {
      expect(document.querySelector('[data-testid="canonical-conversation-route"]')).not.toBeNull()
    })
  })

  it.each([
    ['session', 'legacyAgentSessionId'],
    ['storage', 'legacyStorageKey'],
    ['history', 'legacyChatHistoryId']
  ])('registers the legacy %s resolver route in the desktop root', async (route, sourceKind) => {
    window.location.hash = `#/legacy/${route}/opaque-value`
    render(<TauriApp />)

    await waitFor(() => {
      expect(document.querySelector('[data-testid="legacy-conversation-route"]')).toHaveAttribute(
        'data-source-kind',
        sourceKind
      )
    })
  })
})
