import { describe, expect, it } from 'vitest'
import {
  checkConversationFirstGuardrails,
  type GuardSources,
  stripComments
} from './check-conversation-first-guardrails'

function root(): string {
  return `
import { PortableAppEffects } from '@/app/PortableAppEffects'
import { createPortableRouter } from '@/app/portable-router'
const router = createPortableRouter()
const effects = <PortableAppEffects />
const status = <ConversationHostStatus />
const recovery = <ConversationRecoveryPanel />
`
}

function portableEffects(): string {
  return `
export function PortableAppEffects() {
  useTerminalAutoSave()
  useSessionWorkspaceBootstrap()
  useConversationHostBootstrap()
  useConversationLifecycle()
  useTerminalResourceLifecycle()
  useTerminalRestore()
  useCrashRecovery()
  useTerminalDetachedOutput()
  useCwd()
  useGitBranch()
  useGitStatus()
  useExitCode()
  useContextBarSettings()
  useAppSettingsLoader()
  useAppliedLanguageSync()
  useAppliedColorThemeSync()
  useAppliedUiZoomSync()
  useKeyboardShortcutsLoader()
  useProjectsLoader()
  useProjectsAutoSave()
  useMenuUpdaterListener()
  useUpdateCheck()
  useUpdateToast()
  useVisibilityState()
  useTerminalExitNotification()
  useRemoteProjects()
  useAcpListeners()
  useAcpAgents()
  useAcpHistory()
  useAcpSessionResume()
  useAcpMcp()
  usePreventFileDropNavigation()
  usePreventNativeContextMenu()
  initNotificationPermissions()
}
`
}

function portableRouter(): string {
  return `
export const portableRouteObjects = [
  { path: 'c/:conversationId' },
  { path: 'legacy/session/:legacyValue' },
  { path: 'legacy/storage/:legacyValue' },
  { path: 'legacy/history/:legacyValue' },
  { path: 'snapshots' },
  { path: 'settings' },
  { path: 'preferences' }
]
export function createPortableRouter() {}
`
}

function validSources(): Record<string, string> {
  return {
    'src-tauri/src/conversation/repository.rs': `
pub struct ConversationRepository;
impl ConversationRepository { fn replace_workspace_bytes(&self) {} }
`,
    'src-tauri/src/conversation/session_workspace.rs': `
pub struct SessionWorkspaceV1 {
  pub conversation_id: ConversationId,
  pub revision: u64,
}
pub enum SessionWorkspaceLoadOutcome { Missing }
`,
    'src-tauri/src/remote/host.rs': `
enum CredentialSource {
  Desktop,
  #[cfg(test)]
  Test(String),
}
async fn start() { serve_router().await; }
#[cfg(test)]
mod tests { const TEXT: &str = "kill_all is forbidden"; }
`,
    'src-tauri/src/web/mod.rs': `
pub async fn serve() {
  acp.kill_all_checked().await;
  pty.kill_all().await;
}
pub async fn serve_router() { build_router(); }
async fn shutdown_signal_future() {}
`,
    'src/renderer/App.tsx': root(),
    'src/renderer/TauriApp.tsx': root(),
    'src/renderer/app/PortableAppEffects.tsx': portableEffects(),
    'src/renderer/app/portable-router.tsx': portableRouter(),
    'src/renderer/lib/router-navigate.ts': 'export function navigate() {}',
    'src/renderer/stores/project-store.ts': 'export function selectProject() {}',
    'src/renderer/layouts/WorkspaceLayout.tsx': `
const closeTerminalViewByRecordId = async () => closeTerminalView('terminal')
const requestTerminateTerminal = () => terminateTerminalResource('terminal')
`,
    'src/renderer/components/mobile/MobileChatShell.tsx': 'export function MobileChatShell() {}',
    'src/renderer/stores/conversation-store.ts': 'export const conversationStore = {}',
    'src/renderer/hooks/use-session-workspace-sync.ts': 'export function sync() {}',
    'src/shared/types/session-workspace.types.ts': `
export interface SessionWorkspaceV1 {
  conversationId: ConversationId
  revision: number
}
`
  }
}

function rules(sources: GuardSources): string[] {
  return checkConversationFirstGuardrails(sources).map((item) => item.rule)
}

describe('Conversation-first structural guardrails', () => {
  it('strips line and block comments without losing actionable line numbers', () => {
    const source = `const live = true\n// terminalApi.terminate('comment')\n/*\nkill_all()\n*/\nconst end = true`
    const stripped = stripComments(source)
    expect(stripped).not.toContain("terminalApi.terminate('comment')")
    expect(stripped).not.toContain('kill_all()')
    expect(stripped.split('\n')).toHaveLength(source.split('\n').length)
  })

  it('accepts the locked architecture and explicit terminate-only path', () => {
    expect(checkConversationFirstGuardrails(validSources())).toEqual([])
  })

  it.each([
    [
      'sole-writer',
      'src-tauri/src/conversation/session_workspace.rs',
      `std::fs::write(path.join("workspace.json"), bytes);`
    ],
    [
      'legacy-read-only',
      'src/renderer/hooks/use-session-workspace-sync.ts',
      `workspaceManifestApi.writeManifest(projectId, value)`
    ],
    [
      'workspace-identity',
      'src/shared/types/session-workspace.types.ts',
      `export interface SessionWorkspaceV1 {\n  projectId: string\n}\n`
    ],
    [
      'raw-claim',
      'src/shared/types/session-workspace.types.ts',
      `export interface SessionWorkspaceV1 {\n  claim: string\n}\n`
    ],
    [
      'navigation-preserves-pty',
      'src/renderer/stores/project-store.ts',
      `terminalApi.terminate('terminal-one')`
    ],
    [
      'root-parity',
      'src/renderer/App.tsx',
      root().replace("import { PortableAppEffects } from '@/app/PortableAppEffects'\n", '')
    ],
    [
      'root-parity',
      'src/renderer/app/PortableAppEffects.tsx',
      portableEffects().replace('  useAcpMcp()\n', '')
    ],
    [
      'desktop-shared-live-ownership',
      'src-tauri/src/remote/host.rs',
      `async fn start() { serve_router().await; acp.kill_all(); }`
    ],
    [
      'standalone-owns-shutdown',
      'src-tauri/src/web/mod.rs',
      `pub async fn serve() { pty.kill_all().await; }\npub async fn serve_router() {}\nasync fn shutdown_signal_future() {}`
    ]
  ])('reports %s violations with exact file:line evidence', (rule, file, replacement) => {
    const sources = validSources()
    sources[file] = replacement
    const findings = checkConversationFirstGuardrails(sources).filter((item) => item.rule === rule)
    expect(findings.length).toBeGreaterThan(0)
    expect(findings[0]).toMatchObject({ rule, file })
    expect(findings[0].line).toBeGreaterThan(0)
    expect(`${findings[0].file}:${findings[0].line}`).toMatch(/:\d+$/)
  })

  it('reports a root that duplicates portable effects with exact file:line evidence', () => {
    const sources = validSources()
    sources['src/renderer/TauriApp.tsx'] += `
function AppEffects() {
  useGitStatus()
}
`

    const findings = checkConversationFirstGuardrails(sources).filter(
      (item) => item.rule === 'root-parity' && item.file === 'src/renderer/TauriApp.tsx'
    )

    expect(
      findings.some((item) => item.message.includes('redeclare portable application effects'))
    ).toBe(true)
    expect(findings.some((item) => item.message.includes('duplicate portable effect hooks'))).toBe(
      true
    )
    expect(findings.every((item) => item.line > 0)).toBe(true)
  })

  it('reports a root that duplicates portable routes with exact file:line evidence', () => {
    const sources = validSources()
    sources['src/renderer/App.tsx'] += `
const duplicateRouter = createHashRouter([
  { path: 'settings' }
])
`

    const findings = checkConversationFirstGuardrails(sources).filter(
      (item) => item.rule === 'root-parity' && item.file === 'src/renderer/App.tsx'
    )

    expect(
      findings.some((item) => item.message.includes('redeclare the portable route table'))
    ).toBe(true)
    expect(findings.some((item) => item.message.includes('duplicate a portable route'))).toBe(true)
    expect(findings.every((item) => item.line > 0)).toBe(true)
  })

  it('does not treat forbidden words in comments as live code', () => {
    const sources = validSources()
    sources['src/renderer/stores/project-store.ts'] = `
// terminalApi.terminate('comment only')
/* kill_all(); workspaceManifestApi.writeManifest() */
export function selectProject() {}
`
    expect(rules(sources)).not.toContain('navigation-preserves-pty')
    expect(rules(sources)).not.toContain('legacy-read-only')
  })
})
