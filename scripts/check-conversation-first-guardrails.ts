import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

export interface GuardFinding {
  rule: string
  file: string
  line: number
  message: string
}

export type GuardSources = Readonly<Record<string, string>>

const REQUIRED_SOURCES = [
  '.github/workflows/pr-validation.yml',
  'src-tauri/src/conversation/application.rs',
  'src-tauri/src/conversation/bootstrap.rs',
  'src-tauri/src/conversation/repository.rs',
  'src-tauri/src/conversation/session_workspace.rs',
  'src-tauri/src/conversation/write_authority.rs',
  'src-tauri/src/pty/manager.rs',
  'src-tauri/src/remote/host.rs',
  'src-tauri/src/web/auth.rs',
  'src-tauri/src/web/conversation_api.rs',
  'src-tauri/src/web/conversation_lifecycle_api.rs',
  'src-tauri/src/web/mod.rs',
  'src-tauri/src/web/router.rs',
  'src-tauri/src/web/session_workspace_api.rs',
  'src-tauri/src/web/terminal_ws.rs',
  'src-tauri/src/web/ws.rs',
  'src/renderer/App.tsx',
  'src/renderer/TauriApp.tsx',
  'src/renderer/app/PortableAppEffects.tsx',
  'src/renderer/app/portable-router.tsx',
  'src/renderer/components/mobile/MobileChatShell.tsx',
  'src/renderer/hooks/use-session-workspace-sync.ts',
  'src/renderer/layouts/WorkspaceLayout.tsx',
  'src/renderer/lib/acp-history-persistence.ts',
  'src/renderer/lib/acp-transport.ts',
  'src/renderer/lib/conversation-api.ts',
  'src/renderer/lib/conversation-lifecycle-api.ts',
  'src/renderer/lib/router-navigate.ts',
  'src/renderer/lib/tauri-conversation-api.ts',
  'src/renderer/lib/tauri-session-workspace-api.ts',
  'src/renderer/lib/web-conversation-api.ts',
  'src/renderer/lib/web-session-workspace-api.ts',
  'src/renderer/stores/conversation-store.ts',
  'src/renderer/stores/project-store.ts',
  'src/shared/types/conversation-api.types.ts',
  'src/shared/types/conversation.types.ts',
  'src/shared/types/session-workspace.types.ts',
  'src/shared/types/web-terminal-protocol.types.ts'
] as const

const NAVIGATION_FILES = [
  'src/renderer/App.tsx',
  'src/renderer/TauriApp.tsx',
  'src/renderer/app/PortableAppEffects.tsx',
  'src/renderer/app/portable-router.tsx',
  'src/renderer/components/mobile/MobileChatShell.tsx',
  'src/renderer/lib/router-navigate.ts',
  'src/renderer/stores/conversation-store.ts',
  'src/renderer/stores/project-store.ts'
] as const

const AUTHENTICATED_CONVERSATION_ADAPTERS = [
  'src-tauri/src/web/conversation_api.rs',
  'src-tauri/src/web/conversation_lifecycle_api.rs',
  'src-tauri/src/web/session_workspace_api.rs'
] as const

const RENDERER_CONVERSATION_ADAPTERS = [
  'src/renderer/lib/acp-history-persistence.ts',
  'src/renderer/lib/conversation-lifecycle-api.ts',
  'src/renderer/lib/tauri-conversation-api.ts',
  'src/renderer/lib/tauri-session-workspace-api.ts',
  'src/renderer/lib/web-conversation-api.ts',
  'src/renderer/lib/web-session-workspace-api.ts'
] as const

const REPOSITORY_MUTATORS = [
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
] as const

const PORTABLE_EFFECT_HOOKS = [
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
] as const

function lineNumber(source: string, index: number): number {
  return source.slice(0, index).split('\n').length
}

function finding(
  rule: string,
  file: string,
  source: string,
  index: number,
  message: string
): GuardFinding {
  return { rule, file, line: lineNumber(source, index), message }
}

/** Strip line/block comments while preserving newlines and character offsets. */
export function stripComments(source: string): string {
  let result = ''
  let state: 'code' | 'line' | 'block' | 'single' | 'double' | 'template' = 'code'
  let escaped = false

  for (let index = 0; index < source.length; index += 1) {
    const current = source[index]
    const next = source[index + 1]

    if (state === 'line') {
      if (current === '\n') {
        state = 'code'
        result += '\n'
      } else {
        result += ' '
      }
      continue
    }
    if (state === 'block') {
      if (current === '*' && next === '/') {
        result += '  '
        index += 1
        state = 'code'
      } else {
        result += current === '\n' ? '\n' : ' '
      }
      continue
    }
    if (state !== 'code') {
      result += current
      if (escaped) {
        escaped = false
      } else if (current === '\\') {
        escaped = true
      } else if (
        (state === 'single' && current === "'") ||
        (state === 'double' && current === '"') ||
        (state === 'template' && current === '`')
      ) {
        state = 'code'
      }
      continue
    }

    if (current === '/' && next === '/') {
      result += '  '
      index += 1
      state = 'line'
    } else if (current === '/' && next === '*') {
      result += '  '
      index += 1
      state = 'block'
    } else {
      result += current
      if (current === "'") state = 'single'
      else if (current === '"') state = 'double'
      else if (current === '`') state = 'template'
    }
  }
  return result
}

function blankPreservingLines(source: string): string {
  return source.replace(/[^\n]/g, ' ')
}

/** Remove Rust-only test modules/items while preserving production line offsets. */
export function stripRustTestCode(source: string): string {
  let result = stripComments(source)
  result = result.replace(/#\[cfg\(test\)\]\s*\n\s*use\s+[^;]+;/g, (matched) =>
    blankPreservingLines(matched)
  )
  const testModule = result.search(/\n#\[cfg\(test\)\]\s*\n(?:pub\([^)]*\)\s+)?mod\s+tests\b/)
  if (testModule >= 0) {
    result = `${result.slice(0, testModule)}${blankPreservingLines(result.slice(testModule))}`
  }
  return result
}

function extractDelimited(source: string, startToken: string, endToken: string): string | null {
  const start = source.indexOf(startToken)
  if (start < 0) return null
  const end = source.indexOf(endToken, start + startToken.length)
  return end < 0 ? null : source.slice(start, end)
}

function scanPattern(
  findings: GuardFinding[],
  rule: string,
  file: string,
  source: string,
  pattern: RegExp,
  message: string
): void {
  const flags = pattern.flags.includes('g') ? pattern.flags : `${pattern.flags}g`
  const regex = new RegExp(pattern.source, flags)
  for (const match of source.matchAll(regex)) {
    findings.push(finding(rule, file, source, match.index ?? 0, message))
  }
}

function requireToken(
  findings: GuardFinding[],
  rule: string,
  file: string,
  source: string,
  token: string,
  message: string
): void {
  if (!source.includes(token)) findings.push({ rule, file, line: 1, message })
}

function requirePattern(
  findings: GuardFinding[],
  rule: string,
  file: string,
  source: string,
  pattern: RegExp,
  message: string
): void {
  const regex = new RegExp(pattern.source, pattern.flags.replace('g', ''))
  if (!regex.test(source)) findings.push({ rule, file, line: 1, message })
}

export function checkConversationFirstGuardrails(sources: GuardSources): GuardFinding[] {
  const findings: GuardFinding[] = []
  const stripped = Object.fromEntries(
    Object.entries(sources).map(([file, source]) => [file, stripComments(source)])
  )
  const production = Object.fromEntries(
    Object.entries(sources).map(([file, source]) => [
      file,
      file.endsWith('.rs') ? stripRustTestCode(source) : stripComments(source)
    ])
  )

  for (const file of REQUIRED_SOURCES) {
    if (!(file in sources)) {
      findings.push({
        rule: 'source-inventory',
        file,
        line: 1,
        message: 'required source is missing'
      })
    }
  }

  const authFile = 'src-tauri/src/web/auth.rs'
  const auth = production[authFile] ?? ''
  for (const token of [
    'subtle::ConstantTimeEq',
    'verify_bearer_for_peer',
    'verify_origin',
    'capability_middleware',
    'RemoteCapability::Read',
    'RemoteCapability::Mutate',
    'RemoteCapability::RecoveryInspect'
  ]) {
    requireToken(
      findings,
      'authenticated-remote-access',
      authFile,
      auth,
      token,
      `remote access authority is missing ${token}`
    )
  }
  for (const token of ['"/conversations"', '"/conversation-recovery/"', '"/terminal/ws"']) {
    requireToken(
      findings,
      'anonymous-exposure',
      authFile,
      auth,
      token,
      `protected capability routing is missing ${token}`
    )
  }

  const wsFile = 'src-tauri/src/web/ws.rs'
  const ws = production[wsFile] ?? ''
  requireToken(
    findings,
    'authenticated-remote-access',
    wsFile,
    ws,
    'authority.verify_bearer_for_peer(&payload.token',
    'ACP WebSocket authenticate must verify the supplied credential before admission'
  )
  requireToken(
    findings,
    'authenticated-remote-access',
    wsFile,
    ws,
    'authority.verify_origin(origin)',
    'ACP WebSocket upgrade must verify Origin before admission'
  )

  const acpTransportFile = 'src/renderer/lib/acp-transport.ts'
  const acpTransport = stripped[acpTransportFile] ?? ''
  requireToken(
    findings,
    'authenticated-remote-access',
    acpTransportFile,
    acpTransport,
    'getRemoteAccessCredential()',
    'renderer WebSocket transport must source its credential from the in-memory pairing boundary'
  )
  for (const [file, source] of [
    [authFile, auth],
    [wsFile, ws],
    [acpTransportFile, acpTransport]
  ] as const) {
    scanPattern(
      findings,
      'authenticated-remote-access',
      file,
      source,
      /(?:token|credential)\s*:\s*['"](?:dev|placeholder|changeme)['"]|accept(?:s|ed)?\s+(?:any|every)\s+(?:token|credential)|authentication\s+(?:is\s+)?deferred/gi,
      'placeholder or accept-any remote authentication is forbidden'
    )
  }

  const routerFile = 'src-tauri/src/web/router.rs'
  const router = production[routerFile] ?? ''
  requireToken(
    findings,
    'anonymous-exposure',
    routerFile,
    router,
    '.layer(middleware::from_fn(capability_middleware))',
    'router must install capability middleware before protected Conversation routes are admitted'
  )
  requireToken(
    findings,
    'anonymous-exposure',
    routerFile,
    router,
    '.layer(Extension(authority))',
    'router must inject the exact host-owned remote access authority'
  )

  for (const file of AUTHENTICATED_CONVERSATION_ADAPTERS) {
    const source = production[file] ?? ''
    scanPattern(
      findings,
      'capability-not-peer-ip',
      file,
      source,
      /(?:peer\.ip\(\)\.is_loopback\(\)|check_local_only\s*\()/,
      'Conversation reads and mutations must use end-to-end capability authorization, not peer IP'
    )
    for (const token of [
      'Extension(authority): Extension<Arc<RemoteAccessAuthority>>',
      'Extension(principal): Extension<RemotePrincipal>'
    ]) {
      requireToken(
        findings,
        'capability-not-peer-ip',
        file,
        source,
        token,
        `Conversation adapter is missing authenticated boundary input: ${token}`
      )
    }
  }

  const terminalWsFile = 'src-tauri/src/web/terminal_ws.rs'
  const terminalWs = production[terminalWsFile] ?? ''
  requireToken(
    findings,
    'remote-terminal-intent',
    terminalWsFile,
    terminalWs,
    'TerminalSpawnIntentV1',
    'remote terminal spawn must deserialize the narrow host-authorized intent'
  )
  scanPattern(
    findings,
    'remote-terminal-intent',
    terminalWsFile,
    terminalWs,
    /\bSpawnOptions\b/,
    'remote terminal transport must never accept or construct raw SpawnOptions'
  )
  const ptyManagerFile = 'src-tauri/src/pty/manager.rs'
  const ptyManager = production[ptyManagerFile] ?? ''
  requirePattern(
    findings,
    'remote-terminal-intent',
    ptyManagerFile,
    ptyManager,
    /#\[serde\([^\]]*deny_unknown_fields[^\]]*\)\]\s*pub struct TerminalSpawnIntentV1\b/s,
    'TerminalSpawnIntentV1 must reject unknown raw spawn fields'
  )
  const terminalProtocolFile = 'src/shared/types/web-terminal-protocol.types.ts'
  const terminalProtocol = stripped[terminalProtocolFile] ?? ''
  const terminalIntent =
    extractDelimited(terminalProtocol, 'export interface TerminalSpawnIntentV1', '\n}') ?? ''
  for (const field of ['program', 'args', 'env', 'cwd', 'shell']) {
    scanPattern(
      findings,
      'remote-terminal-intent',
      terminalProtocolFile,
      terminalIntent,
      new RegExp(`^\\s*${field}\\??\\s*:`, 'm'),
      `remote TerminalSpawnIntentV1 must not expose caller-controlled ${field}`
    )
  }

  const repositoryFile = 'src-tauri/src/conversation/repository.rs'
  const repository = production[repositoryFile] ?? ''
  requireToken(
    findings,
    'sole-writer',
    repositoryFile,
    repository,
    'pub struct ConversationRepository',
    'canonical ConversationRepository writer is missing'
  )
  requireToken(
    findings,
    'sole-writer',
    repositoryFile,
    repository,
    'replace_workspace_bytes',
    'SessionWorkspace writes must remain inside ConversationRepository'
  )

  const writeAuthorityFile = 'src-tauri/src/conversation/write_authority.rs'
  const writeAuthority = production[writeAuthorityFile] ?? ''
  for (const token of [
    'pub struct ConversationWriteAuthority',
    'pub struct ConversationWriter',
    'pub(crate) struct RepositoryWritePermit',
    'ReaderPrecedence::HybridLegacyFirst',
    'ConversationErrorCode::LegacyCompatibilityReadOnly',
    'pub(crate) struct MigrationWriter'
  ]) {
    requireToken(
      findings,
      'write-admission',
      writeAuthorityFile,
      writeAuthority,
      token,
      `bootstrap-owned Conversation write admission is missing ${token}`
    )
  }
  requirePattern(
    findings,
    'write-admission',
    writeAuthorityFile,
    writeAuthority,
    /#\[cfg\(test\)\]\s*pub\(crate\) fn for_test\b/s,
    'unrestricted ConversationWriter::for_test construction must remain test-only'
  )
  for (const mutator of REPOSITORY_MUTATORS) {
    const declaration = new RegExp(
      `pub\\(crate\\)\\s+(?:async\\s+)?fn\\s+${mutator}\\s*\\([^)]{0,260}?RepositoryWritePermit`
    )
    requirePattern(
      findings,
      'write-admission',
      repositoryFile,
      repository,
      declaration,
      `repository mutator ${mutator} must require an unforgeable RepositoryWritePermit`
    )
  }
  for (const file of [
    'src-tauri/src/conversation/application.rs',
    'src-tauri/src/conversation/bootstrap.rs',
    'src-tauri/src/conversation/repository.rs',
    'src-tauri/src/conversation/session_workspace.rs',
    'src-tauri/src/web/terminal_ws.rs'
  ]) {
    scanPattern(
      findings,
      'host-service-graph',
      file,
      production[file] ?? '',
      /\blookup_(?:single_)?open\s*\(/,
      'production Conversation and terminal paths must use the exact bootstrap-owned service graph'
    )
  }

  const sessionWorkspaceFile = 'src-tauri/src/conversation/session_workspace.rs'
  const sessionWorkspace = production[sessionWorkspaceFile] ?? ''
  const sessionWorkspaceProduction = sessionWorkspace
  scanPattern(
    findings,
    'sole-writer',
    sessionWorkspaceFile,
    sessionWorkspaceProduction,
    /(?:std::)?fs::(?:write|remove_file|rename)\s*\([^;]*(?:conversation\.json|messages\.jsonl|tool-calls\.jsonl|bindings\.jsonl|attachments\.jsonl|workspace\.json)/s,
    'canonical Conversation files must be mutated through ConversationRepository'
  )
  scanPattern(
    findings,
    'legacy-read-only',
    sessionWorkspaceFile,
    sessionWorkspaceProduction,
    /(?:ChatHistoryStore|SessionPersistence|WorkspaceManifestService)\s*::[^;]*(?:save|write|delete|flush|mark_)/,
    'legacy stores are compatibility readers, not live writers'
  )

  const workspaceTypesFile = 'src/shared/types/session-workspace.types.ts'
  const workspaceTypes = stripped[workspaceTypesFile] ?? ''
  const workspaceInterface =
    extractDelimited(workspaceTypes, 'export interface SessionWorkspaceV1', '\n}') ?? workspaceTypes
  scanPattern(
    findings,
    'workspace-identity',
    workspaceTypesFile,
    workspaceInterface,
    /^\s*(?:projectId|sessionId)\??\s*:/m,
    'SessionWorkspace must be keyed only by conversationId'
  )
  scanPattern(
    findings,
    'raw-claim',
    workspaceTypesFile,
    workspaceTypes,
    /^\s*(?:claim|rawClaim|env|envVars|credentials|terminalOutput)\??\s*:/m,
    'raw claims, environment, credentials, and terminal output must not be persisted'
  )

  const rustWorkspaceBody =
    extractDelimited(
      sessionWorkspace,
      'pub struct SessionWorkspaceV1',
      'pub enum SessionWorkspaceLoadOutcome'
    ) ?? sessionWorkspace
  scanPattern(
    findings,
    'workspace-identity',
    sessionWorkspaceFile,
    rustWorkspaceBody,
    /^\s*pub\s+(?:project_id|session_id)\s*:/m,
    'SessionWorkspaceV1 must be keyed only by conversation_id'
  )
  scanPattern(
    findings,
    'raw-claim',
    sessionWorkspaceFile,
    rustWorkspaceBody,
    /^\s*pub\s+(?:claim|raw_claim|env|env_vars|credentials|terminal_output)\s*:/m,
    'raw claim or secret-bearing terminal state must not be persisted'
  )

  for (const file of NAVIGATION_FILES) {
    const source = stripped[file] ?? ''
    scanPattern(
      findings,
      'navigation-preserves-pty',
      file,
      source,
      /(?:terminalApi\.)?(?:terminate|kill|forceKill)\s*\(|terminateTerminalResource\s*\(|kill_all\s*\(/,
      'navigation, view-close, root unmount, project switch, and shared UI must not terminate PTYs'
    )
  }
  const workspaceLayoutFile = 'src/renderer/layouts/WorkspaceLayout.tsx'
  const workspaceLayout = stripped[workspaceLayoutFile] ?? ''
  const closeViewBody =
    extractDelimited(
      workspaceLayout,
      'const closeTerminalViewByRecordId',
      'const requestTerminateTerminal'
    ) ?? workspaceLayout
  scanPattern(
    findings,
    'navigation-preserves-pty',
    workspaceLayoutFile,
    closeViewBody,
    /(?:terminalApi\.)?(?:terminate|kill|forceKill)\s*\(|terminateTerminalResource\s*\(|kill_all\s*\(/,
    'terminal view-close must not terminate the PTY resource'
  )

  for (const file of [
    'src/renderer/stores/conversation-store.ts',
    'src/renderer/hooks/use-session-workspace-sync.ts'
  ]) {
    const source = stripped[file] ?? ''
    scanPattern(
      findings,
      'legacy-read-only',
      file,
      source,
      /workspaceManifestApi\.(?:writeManifest|deleteManifest)|(?:save|delete)HistorySession\s*\(/,
      'Conversation-first renderer paths must not write legacy stores'
    )
  }

  for (const file of RENDERER_CONVERSATION_ADAPTERS) {
    const source = stripped[file] ?? ''
    requirePattern(
      findings,
      'shared-conversation-id-parser',
      file,
      source,
      /\b(?:isConversationId|parseConversationId)\b/,
      'renderer Conversation adapter must use the shared Rust-compatible ConversationId parser'
    )
    scanPattern(
      findings,
      'shared-conversation-id-parser',
      file,
      source,
      /\b(?:canonicalUuid|canonicalConversationIdPattern)\b|\/\^\[0-9a-f\]\{8\}[^/]+\$\//,
      'renderer Conversation adapter must not own a local UUID validator'
    )
  }

  const coreConversationTypesFile = 'src/shared/types/conversation-api.types.ts'
  const coreConversationTypes = stripped[coreConversationTypesFile] ?? ''
  const coreConversationInterface =
    extractDelimited(coreConversationTypes, 'export interface ConversationApi', '\n}') ?? ''
  for (const method of [
    'getWorkspace',
    'writeWorkspace',
    'resolveRecovery',
    'detachBinding',
    'rebindDetachedBinding',
    'suspendBinding',
    'replaceBinding',
    'deleteConversation'
  ]) {
    scanPattern(
      findings,
      'facade-transport-ownership',
      coreConversationTypesFile,
      coreConversationInterface,
      new RegExp(`\\b${method}\\s*\\(`),
      `core ConversationApi must not duplicate specialized facade method ${method}`
    )
  }
  for (const file of [
    'src/renderer/lib/tauri-conversation-api.ts',
    'src/renderer/lib/web-conversation-api.ts'
  ]) {
    const source = stripped[file] ?? ''
    for (const method of [
      'getWorkspace',
      'writeWorkspace',
      'resolveRecovery',
      'detachBinding',
      'rebindDetachedBinding',
      'suspendBinding',
      'replaceBinding',
      'deleteConversation'
    ]) {
      scanPattern(
        findings,
        'facade-transport-ownership',
        file,
        source,
        new RegExp(`\\b${method}\\s*(?:\\(|:)`),
        `core Conversation transport must not duplicate specialized method ${method}`
      )
    }
  }
  const conversationFacadeFile = 'src/renderer/lib/conversation-api.ts'
  const conversationFacade = stripped[conversationFacadeFile] ?? ''
  for (const token of [
    'sessionWorkspaceApi',
    'conversationLifecycleApi',
    'tauriConversationApi',
    'webConversationApi',
    'createConversationFacadeApi'
  ]) {
    requireToken(
      findings,
      'facade-transport-ownership',
      conversationFacadeFile,
      conversationFacade,
      token,
      `compatibility facade is missing zero-logic delegation through ${token}`
    )
  }
  scanPattern(
    findings,
    'facade-transport-ownership',
    conversationFacadeFile,
    conversationFacade,
    /\binvoke\s*\(|\bfetch\s*\(|new\s+WebSocket\s*\(/,
    'compatibility Conversation facade must not own Tauri, HTTP, or WebSocket transport logic'
  )

  const portableEffectsFile = 'src/renderer/app/PortableAppEffects.tsx'
  const portableEffects = stripped[portableEffectsFile] ?? ''
  for (const token of [
    ...PORTABLE_EFFECT_HOOKS.map((hook) => `${hook}()`),
    'initNotificationPermissions()'
  ]) {
    requireToken(
      findings,
      'root-parity',
      portableEffectsFile,
      portableEffects,
      token,
      `shared portable effects are missing Conversation wiring: ${token}`
    )
  }

  const portableRouterFile = 'src/renderer/app/portable-router.tsx'
  const portableRouter = stripped[portableRouterFile] ?? ''
  for (const token of [
    "path: 'c/:conversationId'",
    "path: 'legacy/session/:legacyValue'",
    "path: 'legacy/storage/:legacyValue'",
    "path: 'legacy/history/:legacyValue'",
    "path: 'snapshots'",
    "path: 'settings'",
    "path: 'preferences'"
  ]) {
    requireToken(
      findings,
      'root-parity',
      portableRouterFile,
      portableRouter,
      token,
      `shared portable router is missing route: ${token}`
    )
  }

  const rootRequirements = [
    "import { PortableAppEffects } from '@/app/PortableAppEffects'",
    "import { createPortableRouter } from '@/app/portable-router'",
    'createPortableRouter()',
    '<PortableAppEffects />',
    '<ConversationHostStatus />',
    '<ConversationRecoveryPanel />'
  ]
  const duplicatedRootPatterns = [
    {
      pattern: /function\s+(?:AppEffects|PortableAppEffects)\s*\(/,
      message: 'renderer root must not redeclare portable application effects'
    },
    {
      pattern: /createHashRouter\s*\(/,
      message: 'renderer root must not redeclare the portable route table'
    },
    {
      pattern:
        /path:\s*['"](?:c\/:conversationId|legacy\/(?:session|storage|history)\/:legacyValue|snapshots|settings|preferences)['"]/,
      message: 'renderer root must not duplicate a portable route declaration'
    },
    {
      pattern: new RegExp(
        `(?:${PORTABLE_EFFECT_HOOKS.join('|')}|initNotificationPermissions)\\s*\\(`
      ),
      message: 'renderer root must not duplicate portable effect hooks'
    }
  ]
  for (const file of ['src/renderer/App.tsx', 'src/renderer/TauriApp.tsx']) {
    const source = stripped[file] ?? ''
    for (const token of rootRequirements) {
      requireToken(
        findings,
        'root-parity',
        file,
        source,
        token,
        `renderer root is missing shared portable wiring: ${token}`
      )
    }
    for (const duplicate of duplicatedRootPatterns) {
      scanPattern(findings, 'root-parity', file, source, duplicate.pattern, duplicate.message)
    }
  }

  const remoteFile = 'src-tauri/src/remote/host.rs'
  // Inline cfg(test) enum variants and constructor branches are valid production-file
  // structure. stripRustTestCode removes only test imports and the terminal test module.
  const remoteProduction = production[remoteFile] ?? ''
  scanPattern(
    findings,
    'desktop-shared-live-ownership',
    remoteFile,
    remoteProduction,
    /(?:kill_all_checked|pty\.kill_all|acp\.kill_all)\s*\(/,
    'desktop shared-live stop must drain serve_router without owning ACP/PTY shutdown'
  )
  requireToken(
    findings,
    'desktop-shared-live-ownership',
    remoteFile,
    remoteProduction,
    'serve_router(',
    'desktop shared-live must call the non-owning serve_router path'
  )

  const webFile = 'src-tauri/src/web/mod.rs'
  const web = production[webFile] ?? ''
  const serveBody = extractDelimited(web, 'pub async fn serve(', 'pub async fn serve_router(') ?? ''
  requireToken(
    findings,
    'standalone-owns-shutdown',
    webFile,
    serveBody,
    'acp.kill_all_checked().await',
    'standalone serve must retain owned ACP shutdown'
  )
  requireToken(
    findings,
    'standalone-owns-shutdown',
    webFile,
    serveBody,
    'pty.kill_all().await',
    'standalone serve must retain owned PTY shutdown'
  )
  const routerBody =
    extractDelimited(web, 'pub async fn serve_router(', 'async fn shutdown_signal_future') ?? ''
  scanPattern(
    findings,
    'desktop-shared-live-ownership',
    webFile,
    routerBody,
    /kill_all(?:_checked)?\s*\(/,
    'serve_router must never own ACP or PTY shutdown'
  )

  const workflowFile = '.github/workflows/pr-validation.yml'
  const workflow = sources[workflowFile] ?? ''
  workflow.split('\n').forEach((line, index) => {
    if (
      !line.trimStart().startsWith('#') &&
      /\bcargo\s+(?:metadata|check|test|clippy|build)\b/.test(line) &&
      !line.includes('--locked')
    ) {
      findings.push({
        rule: 'locked-rust-ci',
        file: workflowFile,
        line: index + 1,
        message: 'every CI cargo metadata/check/test/clippy/build command must use --locked'
      })
    }
  })
  for (const token of [
    'conversation-native-durability:',
    'platform: linux',
    'platform: macos',
    'platform: windows',
    'cargo test --locked conversation::native_durability_tests',
    'cargo build --locked --bin termul-server --features standalone-server',
    'cargo clippy --locked --bin termul-server --features standalone-server -- -D warnings'
  ]) {
    requireToken(
      findings,
      'native-ci-wiring',
      workflowFile,
      workflow,
      token,
      `Conversation native/standalone CI wiring is missing ${token}`
    )
  }

  return findings.sort(
    (left, right) =>
      left.file.localeCompare(right.file) ||
      left.line - right.line ||
      left.rule.localeCompare(right.rule)
  )
}

export function loadRepositorySources(root = process.cwd()): GuardSources {
  return Object.fromEntries(
    REQUIRED_SOURCES.map((file) => [file, readFileSync(resolve(root, file), 'utf8')])
  )
}

export function main(sources: GuardSources = loadRepositorySources()): number {
  const findings = checkConversationFirstGuardrails(sources)
  if (findings.length === 0) {
    console.log(`Conversation-first guardrails passed (${REQUIRED_SOURCES.length} sources)`)
    return 0
  }
  for (const item of findings) {
    console.error(`${item.file}:${item.line} [${item.rule}] ${item.message}`)
  }
  console.error(`Conversation-first guardrails failed with ${findings.length} finding(s)`)
  return 1
}

if (import.meta.main) process.exit(main())
