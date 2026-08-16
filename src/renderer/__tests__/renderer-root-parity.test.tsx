import { cleanup, render, screen, waitFor } from '@testing-library/react'
import type { ComponentType, ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from '../App'
import TauriApp from '../TauriApp'

const { mockPreventDevToolsShortcuts } = vi.hoisted(() => ({
  mockPreventDevToolsShortcuts: vi.fn()
}))

vi.mock('@/app/PortableAppEffects', () => ({
  PortableAppEffects: () => <div data-testid="portable-app-effects" />
}))

vi.mock('@/layouts/WorkspaceLayout', async () => {
  const { Outlet } = await import('react-router-dom')
  return { default: () => <Outlet /> }
})

vi.mock('@/components/conversation/ConversationRoute', () => ({
  ConversationRoute: () => <div data-testid="portable-route" data-component="conversation" />
}))

vi.mock('@/components/ChatRoute', () => ({
  ChatRoute: ({ sourceKind }: { sourceKind: string }) => (
    <div data-testid="portable-route" data-component={`legacy:${sourceKind}`} />
  )
}))

vi.mock('@/pages/WorkspaceDashboard', () => ({
  default: () => <div data-testid="portable-route" data-component="dashboard" />
}))

vi.mock('@/pages/WorkspaceSnapshots', () => ({
  default: () => <div data-testid="portable-route" data-component="snapshots" />
}))

vi.mock('@/pages/ProjectSettings', () => ({
  default: () => <div data-testid="portable-route" data-component="settings" />
}))

vi.mock('@/pages/AppPreferences', () => ({
  default: () => <div data-testid="portable-route" data-component="preferences" />
}))

vi.mock('@/pages/NotFound', () => ({
  default: () => <div data-testid="portable-route" data-component="not-found" />
}))

vi.mock('@/components/conversation/ConversationHostStatus', () => ({
  ConversationHostStatus: () => <div data-testid="conversation-host-status" />
}))

vi.mock('@/components/conversation/ConversationRecoveryPanel', () => ({
  ConversationRecoveryPanel: () => <div data-testid="conversation-recovery-panel" />
}))

vi.mock('@/components/ErrorBoundary', () => ({
  ErrorBoundary: ({ children }: { children: ReactNode }) => <>{children}</>
}))

vi.mock('@/components/GlobalContextMenu', () => ({
  GlobalContextMenu: ({ children }: { children: ReactNode }) => <>{children}</>
}))

vi.mock('@/components/ui/tooltip', () => ({
  TooltipProvider: ({ children }: { children: ReactNode }) => <>{children}</>
}))

vi.mock('@/components/ui/toaster', () => ({ Toaster: () => null }))
vi.mock('@/components/ui/sonner', () => ({ Toaster: () => null }))
vi.mock('@/components/WhatsNewModal', () => ({ WhatsNewModal: () => null }))
vi.mock('@/components/DirectoryPicker', () => ({
  DirectoryPicker: () => <div data-testid="web-directory-picker" />
}))

vi.mock('@/hooks/use-whats-new', () => ({
  useWhatsNew: () => ({
    isOpen: false,
    version: '',
    notes: null,
    htmlUrl: null,
    close: vi.fn()
  })
}))

vi.mock('@/hooks/use-prevent-devtools-shortcuts', () => ({
  usePreventDevToolsShortcuts: mockPreventDevToolsShortcuts
}))

vi.mock('@/hooks/use-window-state', () => ({ useWindowState: () => false }))
vi.mock('@/lib/platform', () => ({ isWindows: false }))
vi.mock('@/lib/tauri-runtime', () => ({ isTauriContext: () => false }))

const routeCases = [
  ['/', 'dashboard'],
  ['/c/018f7a1c-1b4d-7c8a-9f01-0123456789ab', 'conversation'],
  ['/legacy/session/opaque-value', 'legacy:legacyAgentSessionId'],
  ['/legacy/storage/opaque-value', 'legacy:legacyStorageKey'],
  ['/legacy/history/opaque-value', 'legacy:legacyChatHistoryId'],
  ['/snapshots', 'snapshots'],
  ['/settings', 'settings'],
  ['/preferences', 'preferences'],
  ['/missing-route', 'not-found']
] as const

async function navigateRoot(
  Root: ComponentType,
  path: string,
  expected: string
): Promise<{
  route: string
  hasPortableEffects: boolean
  hasHostStatus: boolean
  hasRecoveryPanel: boolean
}> {
  window.location.hash = `#${path}`
  window.dispatchEvent(new HashChangeEvent('hashchange'))
  const view = render(<Root />)
  await waitFor(() => {
    expect(screen.getByTestId('portable-route')).toHaveAttribute('data-component', expected)
  })
  const result = {
    route: screen.getByTestId('portable-route').getAttribute('data-component') ?? '',
    hasPortableEffects: screen.queryByTestId('portable-app-effects') !== null,
    hasHostStatus: screen.queryByTestId('conversation-host-status') !== null,
    hasRecoveryPanel: screen.queryByTestId('conversation-recovery-panel') !== null
  }
  view.unmount()
  cleanup()
  return result
}

afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})

describe('renderer root runtime parity', () => {
  it.each(routeCases)('navigates %s to the same portable %s component', async (path, expected) => {
    const web = await navigateRoot(App, path, expected)
    const native = await navigateRoot(TauriApp, path, expected)

    expect(web).toEqual({
      route: expected,
      hasPortableEffects: true,
      hasHostStatus: true,
      hasRecoveryPanel: true
    })
    expect(native).toEqual(web)
  })

  it('keeps browser and native wrappers platform-specific around the portable shell', async () => {
    window.location.hash = '#/'
    const web = render(<App />)
    await waitFor(() => expect(screen.queryByTestId('portable-app-effects')).not.toBeNull())
    expect(screen.queryByTestId('web-directory-picker')).not.toBeNull()
    expect(mockPreventDevToolsShortcuts).not.toHaveBeenCalled()
    web.unmount()
    cleanup()

    render(<TauriApp />)
    await waitFor(() => expect(screen.queryByTestId('portable-app-effects')).not.toBeNull())
    expect(screen.queryByTestId('web-directory-picker')).toBeNull()
    expect(mockPreventDevToolsShortcuts).toHaveBeenCalledTimes(1)
  })
})
