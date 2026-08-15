import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

const rendererRoot = join(__dirname, '..')
const appSource = readFileSync(join(rendererRoot, 'App.tsx'), 'utf8')
const tauriSource = readFileSync(join(rendererRoot, 'TauriApp.tsx'), 'utf8')

const portableRoutes = [
  'c/:conversationId',
  'legacy/session/:legacyValue',
  'legacy/storage/:legacyValue',
  'legacy/history/:legacyValue',
  'snapshots',
  'settings',
  'preferences'
] as const

const portableWiring = [
  'useSessionWorkspaceBootstrap()',
  'useConversationHostBootstrap()',
  'useConversationLifecycle()',
  'useTerminalResourceLifecycle()',
  '<ConversationHostStatus />',
  '<ConversationRecoveryPanel />',
  '<GlobalContextMenu>',
  '<ErrorBoundary context="appRoot">'
] as const

function routeSet(source: string): string[] {
  return portableRoutes.filter((route) => source.includes(`path: '${route}'`)).sort()
}

function wiringSet(source: string): string[] {
  return portableWiring.filter((token) => source.includes(token)).sort()
}

describe('renderer root Conversation parity', () => {
  it('loads both root modules as executable React components', async () => {
    const [app, tauri] = await Promise.all([import('../App'), import('../TauriApp')])
    expect(typeof app.default).toBe('function')
    expect(typeof tauri.default).toBe('function')
  }, 15_000)

  it('registers identical portable canonical and legacy routes', () => {
    expect(routeSet(appSource)).toEqual([...portableRoutes].sort())
    expect(routeSet(tauriSource)).toEqual([...portableRoutes].sort())
    expect(routeSet(appSource)).toEqual(routeSet(tauriSource))
  })

  it('mounts identical Conversation providers, hooks, host status, and recovery UI', () => {
    expect(wiringSet(appSource)).toEqual([...portableWiring].sort())
    expect(wiringSet(tauriSource)).toEqual([...portableWiring].sort())
    expect(wiringSet(appSource)).toEqual(wiringSet(tauriSource))
  })

  it('keeps platform-only differences explicit without changing Conversation behavior', () => {
    expect(appSource).toContain('!isTauriContext() && <DirectoryPicker />')
    expect(tauriSource).not.toContain('<DirectoryPicker />')
    for (const token of ['ConversationRoute', 'ChatRoute', 'ConversationHostStatus']) {
      expect(appSource).toContain(token)
      expect(tauriSource).toContain(token)
    }
  })

  it('does not terminate PTYs from either renderer root', () => {
    for (const [file, source] of [
      ['App.tsx', appSource],
      ['TauriApp.tsx', tauriSource]
    ] as const) {
      expect(source, file).not.toMatch(/terminalApi\.(?:terminate|kill)\s*\(/)
      expect(source, file).not.toMatch(/terminateTerminalResource\s*\(/)
    }
  })
})
