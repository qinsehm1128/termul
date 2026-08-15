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
  'src-tauri/src/conversation/repository.rs',
  'src-tauri/src/conversation/session_workspace.rs',
  'src-tauri/src/remote/host.rs',
  'src-tauri/src/web/mod.rs',
  'src/renderer/App.tsx',
  'src/renderer/TauriApp.tsx',
  'src/renderer/lib/router-navigate.ts',
  'src/renderer/stores/project-store.ts',
  'src/renderer/layouts/WorkspaceLayout.tsx',
  'src/renderer/components/mobile/MobileChatShell.tsx',
  'src/renderer/stores/conversation-store.ts',
  'src/renderer/hooks/use-session-workspace-sync.ts',
  'src/shared/types/session-workspace.types.ts'
] as const

const NAVIGATION_FILES = [
  'src/renderer/App.tsx',
  'src/renderer/TauriApp.tsx',
  'src/renderer/lib/router-navigate.ts',
  'src/renderer/stores/project-store.ts'
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

export function checkConversationFirstGuardrails(sources: GuardSources): GuardFinding[] {
  const findings: GuardFinding[] = []
  const stripped = Object.fromEntries(
    Object.entries(sources).map(([file, source]) => [file, stripComments(source)])
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

  const repositoryFile = 'src-tauri/src/conversation/repository.rs'
  const repository = stripped[repositoryFile] ?? ''
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

  const sessionWorkspaceFile = 'src-tauri/src/conversation/session_workspace.rs'
  const sessionWorkspace = stripped[sessionWorkspaceFile] ?? ''
  const sessionWorkspaceProduction = sessionWorkspace.split('#[cfg(test)]')[0] ?? sessionWorkspace
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

  const rootRequirements = [
    "path: 'c/:conversationId'",
    "path: 'legacy/session/:legacyValue'",
    "path: 'legacy/storage/:legacyValue'",
    "path: 'legacy/history/:legacyValue'",
    'useSessionWorkspaceBootstrap()',
    'useConversationHostBootstrap()',
    'useConversationLifecycle()',
    'useTerminalResourceLifecycle()',
    '<ConversationHostStatus />',
    '<ConversationRecoveryPanel />'
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
        `renderer root is missing portable Conversation wiring: ${token}`
      )
    }
  }

  const remoteFile = 'src-tauri/src/remote/host.rs'
  const remote = stripped[remoteFile] ?? ''
  const remoteProduction = remote.split('#[cfg(test)]')[0] ?? remote
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
  const web = stripped[webFile] ?? ''
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

export function main(): number {
  const findings = checkConversationFirstGuardrails(loadRepositorySources())
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
