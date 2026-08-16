import { describe, expect, it, vi } from 'vitest'
import {
  checkConversationFirstGuardrails,
  type GuardSources,
  main,
  stripComments,
  stripRustTestCode
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

const repositoryMutators = [
  'replace_workspace_bytes',
  'create_conversation',
  'update_metadata',
  'append_event',
  'bind_agent_session',
  'detach_agent_binding',
  'rebind_detached_binding',
  'suspend_agent_binding',
  'replace_agent_binding',
  'refresh_lifecycle_catalog',
  'append_project_attachment',
  'detach_project_attachment',
  'attach_project_cas',
  'detach_project_cas',
  'update_execution_target_cas',
  'write_provenance',
  'sync_conversation',
  'mark_deleted',
  'tombstone_conversation_locked',
  'mark_lifecycle_recovery_required_locked',
  'clear_recovery_item'
]

function lockedWorkflow(): string {
  return `
jobs:
  conversation-native-durability:
    matrix:
      include:
        - platform: linux
        - platform: macos
        - platform: windows
    steps:
      - run: cargo metadata --locked --format-version 1
      - run: cargo test --locked conversation::native_durability_tests
      - run: cargo check --locked --all-targets
      - run: cargo clippy --locked --all-targets -- -D warnings
      - run: cargo build --locked --bin termul-server --features standalone-server
      - run: cargo clippy --locked --bin termul-server --features standalone-server -- -D warnings
`
}

function authenticatedAdapter(): string {
  return `
fn handler(
  Extension(authority): Extension<Arc<RemoteAccessAuthority>>,
  Extension(principal): Extension<RemotePrincipal>,
) { authority.authorize(&principal, RemoteCapability::Mutate); }
`
}

function sharedParserAdapter(): string {
  return `
import { isConversationId } from '@shared/types/conversation.types'
export const valid = isConversationId(value)
`
}

function validSources(): Record<string, string> {
  const repository = `
pub struct ConversationRepository;
${repositoryMutators
  .map((mutator) => `pub(crate) fn ${mutator}(permit: &RepositoryWritePermit) { let _ = permit; }`)
  .join('\n')}
`
  return {
    '.github/workflows/pr-validation.yml': lockedWorkflow(),
    'src-tauri/src/conversation/application.rs': 'pub struct ConversationApplicationService;',
    'src-tauri/src/conversation/bootstrap.rs': 'pub struct BootstrapOutcome;',
    'src-tauri/src/conversation/repository.rs': repository,
    'src-tauri/src/conversation/session_workspace.rs': `
pub struct SessionWorkspaceV1 {
  pub conversation_id: ConversationId,
  pub revision: u64,
}
pub enum SessionWorkspaceLoadOutcome { Missing }
`,
    'src-tauri/src/conversation/write_authority.rs': `
pub struct ConversationWriteAuthority;
pub struct ConversationWriter;
pub(crate) struct RepositoryWritePermit;
pub(crate) struct MigrationWriter;
const PRECEDENCE: ReaderPrecedence = ReaderPrecedence::HybridLegacyFirst;
const ERROR: ConversationErrorCode = ConversationErrorCode::LegacyCompatibilityReadOnly;
#[cfg(test)]
pub(crate) fn for_test() {}
`,
    'src-tauri/src/pty/manager.rs': `
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSpawnIntentV1 { conversation_id: ConversationId }
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
    'src-tauri/src/web/auth.rs': `
use subtle::ConstantTimeEq;
enum RemoteCapability { Read, Mutate, RecoveryInspect }
fn verify_bearer_for_peer() {}
fn verify_origin() {}
fn capability_middleware() {
  let _ = RemoteCapability::Read;
  let _ = RemoteCapability::Mutate;
  let _ = RemoteCapability::RecoveryInspect;
  let protected = ["/conversations", "/conversation-recovery/", "/terminal/ws"];
}
`,
    'src-tauri/src/web/conversation_api.rs': authenticatedAdapter(),
    'src-tauri/src/web/conversation_lifecycle_api.rs': authenticatedAdapter(),
    'src-tauri/src/web/mod.rs': `
pub async fn serve() {
  acp.kill_all_checked().await;
  pty.kill_all().await;
}
pub async fn serve_router() { build_router(); }
async fn shutdown_signal_future() {}
`,
    'src-tauri/src/web/router.rs': `
fn router() {
  Router::new()
    .layer(middleware::from_fn(capability_middleware))
    .layer(Extension(authority));
}
`,
    'src-tauri/src/web/session_workspace_api.rs': authenticatedAdapter(),
    'src-tauri/src/web/terminal_ws.rs': `
use crate::pty::manager::TerminalSpawnIntentV1;
fn dispatch(payload: Value) {
  let _: TerminalSpawnIntentV1 = serde_json::from_value(payload).unwrap();
}
#[cfg(test)]
use crate::pty::manager::SpawnOptions;
#[cfg(test)]
mod tests { fn accepts_raw_for_local_fixture() { let _ = SpawnOptions::default(); } }
`,
    'src-tauri/src/web/ws.rs': `
fn upgrade(origin: Origin) { authority.verify_origin(origin); }
fn authenticate(payload: Payload, peer: Peer) {
  authority.verify_bearer_for_peer(&payload.token, peer.ip());
  *authed = true;
}
`,
    'src/renderer/App.tsx': root(),
    'src/renderer/TauriApp.tsx': root(),
    'src/renderer/app/PortableAppEffects.tsx': portableEffects(),
    'src/renderer/app/portable-router.tsx': portableRouter(),
    'src/renderer/components/mobile/MobileChatShell.tsx': 'export function MobileChatShell() {}',
    'src/renderer/hooks/use-session-workspace-sync.ts': 'export function sync() {}',
    'src/renderer/layouts/WorkspaceLayout.tsx': `
const closeTerminalViewByRecordId = async () => closeTerminalView('terminal')
const requestTerminateTerminal = () => terminateTerminalResource('terminal')
`,
    'src/renderer/lib/acp-history-persistence.ts': sharedParserAdapter(),
    'src/renderer/lib/acp-transport.ts': `
function getRemoteAccessCredential() { return memoryCredential }
const payload = { token: getRemoteAccessCredential() }
`,
    'src/renderer/lib/conversation-api.ts': `
const delegates = {
  sessionWorkspaceApi,
  conversationLifecycleApi,
  tauriConversationApi,
  webConversationApi,
  createConversationFacadeApi
}
`,
    'src/renderer/lib/conversation-lifecycle-api.ts': sharedParserAdapter(),
    'src/renderer/lib/router-navigate.ts': 'export function navigate() {}',
    'src/renderer/lib/tauri-conversation-api.ts': sharedParserAdapter(),
    'src/renderer/lib/tauri-session-workspace-api.ts': sharedParserAdapter(),
    'src/renderer/lib/web-conversation-api.ts': sharedParserAdapter(),
    'src/renderer/lib/web-session-workspace-api.ts': sharedParserAdapter(),
    'src/renderer/stores/conversation-store.ts': 'export const conversationStore = {}',
    'src/renderer/stores/project-store.ts': 'export function selectProject() {}',
    'src/shared/types/conversation-api.types.ts': `
export interface ConversationApi {
  listConversations(): Promise<void>
}
`,
    'src/shared/types/conversation.types.ts': `
export function isConversationId(value: string): boolean { return value.length > 0 }
`,
    'src/shared/types/session-workspace.types.ts': `
export interface SessionWorkspaceV1 {
  conversationId: ConversationId
  revision: number
}
`,
    'src/shared/types/web-terminal-protocol.types.ts': `
export interface TerminalSpawnIntentV1 {
  conversationId: ConversationId
  cwdSource: 'workspace' | 'executionTarget'
  cols: number
  rows: number
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

  it('removes Rust cfg(test) imports and modules without hiding production lines', () => {
    const source = `fn production() {}\n#[cfg(test)]\nuse crate::SpawnOptions;\n#[cfg(test)]\nmod tests { fn raw() { SpawnOptions::default(); } }`
    const stripped = stripRustTestCode(source)
    expect(stripped).toContain('fn production() {}')
    expect(stripped).not.toContain('SpawnOptions')
    expect(stripped.split('\n')).toHaveLength(source.split('\n').length)
  })

  it('accepts the locked architecture and explicit terminate-only path', () => {
    expect(checkConversationFirstGuardrails(validSources())).toEqual([])
  })

  it('does not let cfg(test) repository fixtures satisfy production write admission', () => {
    const sources = validSources()
    sources['src-tauri/src/conversation/repository.rs'] = sources[
      'src-tauri/src/conversation/repository.rs'
    ]
      .replace(
        'pub(crate) fn append_event(permit: &RepositoryWritePermit) { let _ = permit; }',
        'pub(crate) fn append_event() {}'
      )
      .concat(`
#[cfg(test)]
mod tests {
  pub(crate) fn append_event(permit: &RepositoryWritePermit) { let _ = permit; }
}
`)

    expect(
      checkConversationFirstGuardrails(sources).some(
        (item) => item.rule === 'write-admission' && item.message.includes('mutator append_event')
      )
    ).toBe(true)
  })

  it('returns process-compatible exit semantics with sanitized file:line failures', () => {
    const log = vi.spyOn(console, 'log').mockImplementation(() => undefined)
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    expect(main(validSources())).toBe(0)

    const invalid = validSources()
    invalid['src-tauri/src/web/terminal_ws.rs'] = `fn run() { let raw = SpawnOptions::default(); }`
    expect(main(invalid)).toBe(1)
    expect(error).toHaveBeenCalledWith(
      expect.stringMatching(/^src-tauri\/src\/web\/terminal_ws\.rs:\d+ \[remote-terminal-intent\]/)
    )

    log.mockRestore()
    error.mockRestore()
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
    ],
    [
      'authenticated-remote-access',
      'src/renderer/lib/acp-transport.ts',
      `function getRemoteAccessCredential() { return 'ignored' }\nconst payload = { token: 'dev' }`
    ],
    [
      'capability-not-peer-ip',
      'src-tauri/src/web/conversation_lifecycle_api.rs',
      `${authenticatedAdapter()}\nfn bypass(peer: Peer) { if peer.ip().is_loopback() { mutate(); } }`
    ],
    [
      'anonymous-exposure',
      'src-tauri/src/web/auth.rs',
      validSources()['src-tauri/src/web/auth.rs'].replace('"/conversations", ', '')
    ],
    [
      'remote-terminal-intent',
      'src-tauri/src/web/terminal_ws.rs',
      `use crate::pty::manager::TerminalSpawnIntentV1;\nfn run() { let raw = SpawnOptions::default(); }`
    ],
    [
      'host-service-graph',
      'src-tauri/src/conversation/application.rs',
      `fn open() { ConversationRepository::lookup_single_open(); }`
    ],
    [
      'write-admission',
      'src-tauri/src/conversation/repository.rs',
      validSources()['src-tauri/src/conversation/repository.rs'].replace(
        'pub(crate) fn append_event(permit: &RepositoryWritePermit)',
        'pub(crate) fn append_event()'
      )
    ],
    [
      'shared-conversation-id-parser',
      'src/renderer/lib/conversation-lifecycle-api.ts',
      `const canonicalUuid = /^[0-9a-f]{8}-[0-9a-f-]+$/`
    ],
    [
      'facade-transport-ownership',
      'src/shared/types/conversation-api.types.ts',
      `export interface ConversationApi {\n  getWorkspace(): Promise<void>\n}`
    ],
    [
      'locked-rust-ci',
      '.github/workflows/pr-validation.yml',
      lockedWorkflow().replace('cargo check --locked', 'cargo check')
    ],
    [
      'native-ci-wiring',
      '.github/workflows/pr-validation.yml',
      lockedWorkflow().replace('        - platform: windows\n', '')
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
