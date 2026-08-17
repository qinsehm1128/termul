import { describe, expect, it, vi } from 'vitest'
import {
  checkConversationFirstGuardrails,
  type GuardSources,
  main,
  stripComments,
  stripRustTestCode
} from './check-conversation-first-guardrails'

const hooks = [
  'useTerminalAutoSave',
  'useSessionWorkspaceBootstrap',
  'useConversationHostBootstrap',
  'useConversationLifecycle',
  'useTerminalResourceLifecycle',
  'useTerminalRestore',
  'useCrashRecovery',
  'useTerminalDetachedOutput',
  'useCwd',
  'useGitBranch',
  'useGitStatus',
  'useExitCode',
  'useContextBarSettings',
  'useAppSettingsLoader',
  'useAppliedLanguageSync',
  'useAppliedColorThemeSync',
  'useAppliedUiZoomSync',
  'useKeyboardShortcutsLoader',
  'useProjectsLoader',
  'useProjectsAutoSave',
  'useMenuUpdaterListener',
  'useUpdateCheck',
  'useUpdateToast',
  'useVisibilityState',
  'useTerminalExitNotification',
  'useRemoteProjects',
  'useAcpListeners',
  'useAcpAgents',
  'useAcpHistory',
  'useAcpSessionResume',
  'useAcpMcp',
  'usePreventFileDropNavigation',
  'usePreventNativeContextMenu'
]

function root(): string {
  return `
import { PortableAppEffects as Effects } from '@/app/PortableAppEffects'
import { createPortableRouter as buildRouter } from '@/app/portable-router'
import { ConversationHostStatus as Status } from '@/components/conversation/ConversationHostStatus'
import { ConversationRecoveryPanel as Recovery } from '@/components/conversation/ConversationRecoveryPanel'
const makeRouter = buildRouter
const router = makeRouter()
export default function Root() {
  return <><Effects /><Status /><Recovery /></>
}
`
}

function portableEffects(): string {
  return `
${hooks.map((hook) => `import { ${hook} as ${hook}Alias } from '@/hooks/${hook}'`).join('\n')}
import { initNotificationPermissions as initializeNotifications } from '@/lib/tauri-notification-api'
export function PortableAppEffects() {
${hooks.map((hook) => `  ${hook}Alias()`).join('\n')}
  initializeNotifications()
  return null
}
`
}

function portableRouter(): string {
  return `
import { createHashRouter } from 'react-router-dom'
export const portableRouteObjects = [{ children: [
  { path: 'c/:conversationId' },
  { path: 'legacy/session/:legacyValue' },
  { path: 'legacy/storage/:legacyValue' },
  { path: 'legacy/history/:legacyValue' },
  { path: 'snapshots' },
  { path: 'settings' },
  { path: 'preferences' }
]}]
export function createPortableRouter() { return createHashRouter(portableRouteObjects) }
`
}

function workflow(extra = ''): string {
  return `
jobs:
  conversation-native-durability:
    strategy:
      matrix:
        include:
          - platform: linux
          - platform: macos
          - platform: windows
    steps:
      - run: cargo test --locked conversation::native_durability_tests
      - run: cargo test --locked --test conversation_first_guardrails
  standalone-server-build:
    steps:
      - run: cargo build --locked --bin termul-server --features standalone-server
      - run: cargo clippy --locked --bin termul-server --features standalone-server -- -D warnings
${extra}`
}

function parserAdapter(): string {
  return `
import { isConversationId as validConversationId } from '@shared/types/conversation.types'
export const valid = validConversationId(value)
`
}

function validSources(): Record<string, string> {
  return {
    '.github/workflows/pr-validation.yml': workflow(),
    '.github/workflows/other.yaml':
      'jobs:\n  check:\n    steps:\n      - run: cargo check --locked --all-targets\n',
    'src/renderer/App.tsx': root(),
    'src/renderer/TauriApp.tsx': root(),
    'src/renderer/app/PortableAppEffects.tsx': portableEffects(),
    'src/renderer/app/portable-router.tsx': portableRouter(),
    'src/renderer/lib/acp-history-persistence.ts': `
import { isConversationId } from '@shared/types/conversation.types'
import { acpHistoryApi as history } from '@/lib/acp-history-api'
const ok = isConversationId(id)
export async function page(mode: string) {
  if (mode === 'server') return transport.getSessionPayloadPage(id, 0, 250)
  return history.getPage(id, 0, 250)
}
`,
    'src/renderer/lib/conversation-lifecycle-api.ts': parserAdapter(),
    'src/renderer/lib/tauri-conversation-api.ts': parserAdapter(),
    'src/renderer/lib/tauri-session-workspace-api.ts': parserAdapter(),
    'src/renderer/lib/web-conversation-api.ts': parserAdapter(),
    'src/renderer/lib/web-session-workspace-api.ts': parserAdapter(),
    'src/renderer/lib/conversation-api.ts': `
import { sessionWorkspaceApi } from './session-workspace-api'
import { conversationLifecycleApi } from './conversation-lifecycle-api'
import { tauriConversationApi } from './tauri-conversation-api'
import { webConversationApi } from './web-conversation-api'
export const conversationApi = { sessionWorkspaceApi, conversationLifecycleApi, tauriConversationApi, webConversationApi }
`,
    'src/renderer/lib/acp-transport.ts': `
import { getRemoteAccessCredential as credential } from './remote-access-credential'
export const payload = { token: credential() }
`,
    'src/shared/types/session-workspace.types.ts': `
export interface SessionWorkspaceV1 { conversationId: string; revision: number }
`,
    'src/shared/types/web-terminal-protocol.types.ts': `
export interface TerminalSpawnIntentV1 { conversationId: string; cols: number; rows: number }
`
  }
}

function findings(sources: GuardSources, rule: string) {
  return checkConversationFirstGuardrails(sources).filter((item) => item.rule === rule)
}

describe('Conversation-first semantic guardrails', () => {
  it('accepts imported aliases and structural JSX/calls/routes', () => {
    expect(checkConversationFirstGuardrails(validSources())).toEqual([])
  })

  it('ignores comments and string decoys but rejects a missing real root node', () => {
    const sources = validSources()
    sources['src/renderer/App.tsx'] = root().replace(
      '<Effects />',
      '{/* <Effects /> */}{"<PortableAppEffects />"}'
    )
    const result = findings(sources, 'root-parity')
    expect(result.some((item) => item.file === 'src/renderer/App.tsx')).toBe(true)
  })

  it('detects renamed teardown aliases inside a moved navigation helper with exact evidence', () => {
    const sources = validSources()
    sources['src/renderer/moved/navigation-owner.ts'] = `
import { terminalApi } from '@/lib/terminal-api'
const dispose = terminalApi.terminate
export function selectProject() { return dispose('pty') }
`
    const result = findings(sources, 'navigation-preserves-pty')
    expect(result).toHaveLength(1)
    expect(result[0]).toMatchObject({ file: 'src/renderer/moved/navigation-owner.ts' })
    expect(result[0].line).toBeGreaterThan(0)
  })

  it('detects one-hop helper indirection without being spoofed by comments', () => {
    const sources = validSources()
    sources['src/renderer/moved/project-navigation.ts'] = `
const stop = () => terminalApi.terminate('pty')
export function switchProject() { return stop() }
// terminalApi.terminate('comment')
const decoy = "terminalApi.terminate('string')"
`
    expect(findings(sources, 'navigation-preserves-pty')).toHaveLength(1)
  })

  it('discovers a newly added workflow path and reports its job/step and line', () => {
    const sources = validSources()
    sources['.github/workflows/new-active.yaml'] = `
jobs:
  fresh:
    steps:
      - name: unlocked
        run: cargo check --all-targets
`
    const result = findings(sources, 'locked-rust-ci')
    expect(result).toHaveLength(1)
    expect(result[0]).toMatchObject({ file: '.github/workflows/new-active.yaml' })
    expect(result[0].message).toContain('job=fresh')
    expect(result[0].line).toBeGreaterThan(0)

    sources['.github/workflows/new-active.yaml'] = sources[
      '.github/workflows/new-active.yaml'
    ].replace('cargo check', 'cargo check --locked')
    expect(findings(sources, 'locked-rust-ci')).toEqual([])
  })

  it('checks multiline YAML run scalars semantically', () => {
    const sources = validSources()
    sources['.github/workflows/folded.yml'] = `
jobs:
  build:
    steps:
      - run: >
          cargo build --release --bin termul-server
          --features standalone-server
`
    expect(findings(sources, 'locked-rust-ci')).toHaveLength(1)
  })

  it('requires the real history paging facades, not token-shaped text', () => {
    const sources = validSources()
    sources['src/renderer/lib/acp-history-persistence.ts'] = `
import { isConversationId } from '@shared/types/conversation.types'
import { acpHistoryApi } from '@/lib/acp-history-api'
const ok = isConversationId(id)
// acpHistoryApi.getPage(id, 0, 250)
const decoy = 'getSessionPayloadPage'
`
    expect(findings(sources, 'history-paging-facade')).toHaveLength(2)
  })

  it('rejects raw remote spawn fields and placeholder credentials as AST nodes', () => {
    const sources = validSources()
    sources['src/shared/types/web-terminal-protocol.types.ts'] = `
export interface TerminalSpawnIntentV1 { conversationId: string; shell?: string }
`
    sources['src/renderer/lib/acp-transport.ts'] = `
function getRemoteAccessCredential() { return memory }
const payload = { token: 'dev' }
`
    expect(findings(sources, 'remote-terminal-intent')).toHaveLength(1)
    expect(findings(sources, 'authenticated-remote-access')).toHaveLength(2)
  })

  it('retains process-compatible sanitized file:line output', () => {
    const log = vi.spyOn(console, 'log').mockImplementation(() => undefined)
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    expect(main(validSources())).toBe(0)
    const sources = validSources()
    sources['.github/workflows/new-active.yaml'] =
      'jobs:\n  x:\n    steps:\n      - run: cargo test\n'
    expect(main(sources)).toBe(1)
    expect(error).toHaveBeenCalledWith(
      expect.stringMatching(/^\.github\/workflows\/new-active\.yaml:\d+ \[locked-rust-ci\]/)
    )
    log.mockRestore()
    error.mockRestore()
  })

  it('keeps compatibility stripping helpers line-stable', () => {
    const source = `const live = true\n// kill_all()\n/* terminate() */\nconst end = true`
    const stripped = stripComments(source)
    expect(stripped).not.toContain('kill_all')
    expect(stripped).not.toContain('terminate')
    expect(stripped.split('\n')).toHaveLength(source.split('\n').length)

    const rust = `fn live() {}\n#[cfg(test)]\nmod tests { fn kill_all() {} }`
    const production = stripRustTestCode(rust)
    expect(production).toContain('fn live() {}')
    expect(production).not.toContain('kill_all')
    expect(production.split('\n')).toHaveLength(rust.split('\n').length)
  })
})
