import { readdirSync, readFileSync } from 'node:fs'
import { extname, join, relative, resolve, sep } from 'node:path'
import { ts } from '@ts-morph/common'
import { isMap, isScalar, isSeq, LineCounter, parseDocument, type Node as YamlNode } from 'yaml'

export interface GuardFinding {
  rule: string
  file: string
  line: number
  message: string
}

export type GuardSources = Readonly<Record<string, string>>

interface SemanticSymbol {
  module: string
  exported: string
}

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

const PORTABLE_ROUTES = [
  'c/:conversationId',
  'legacy/session/:legacyValue',
  'legacy/storage/:legacyValue',
  'legacy/history/:legacyValue',
  'snapshots',
  'settings',
  'preferences'
] as const

const SHARED_PARSER_ADAPTER_SUFFIXES = [
  'src/renderer/lib/acp-history-persistence.ts',
  'src/renderer/lib/conversation-lifecycle-api.ts',
  'src/renderer/lib/tauri-conversation-api.ts',
  'src/renderer/lib/tauri-session-workspace-api.ts',
  'src/renderer/lib/web-conversation-api.ts',
  'src/renderer/lib/web-session-workspace-api.ts'
] as const

function normalizePath(path: string): string {
  return path.split(sep).join('/')
}

function lineAt(sourceFile: ts.SourceFile, node: ts.Node): number {
  return sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1
}

function addFinding(
  findings: GuardFinding[],
  rule: string,
  file: string,
  sourceFile: ts.SourceFile,
  node: ts.Node,
  message: string
): void {
  findings.push({ rule, file, line: lineAt(sourceFile, node), message })
}

function scriptKind(file: string): ts.ScriptKind {
  return file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS
}

function isTypeScriptFile(file: string): boolean {
  return /\.(?:ts|tsx)$/.test(file) && !/\.d\.ts$/.test(file)
}

class TypeScriptModel {
  readonly sourceFile: ts.SourceFile
  private readonly imports = new Map<string, SemanticSymbol>()
  private readonly aliases = new Map<string, ts.Expression>()

  constructor(
    readonly file: string,
    readonly source: string
  ) {
    this.sourceFile = ts.createSourceFile(
      file,
      source,
      ts.ScriptTarget.Latest,
      true,
      scriptKind(file)
    )
    this.indexBindings()
  }

  private indexBindings(): void {
    for (const statement of this.sourceFile.statements) {
      if (ts.isImportDeclaration(statement) && ts.isStringLiteral(statement.moduleSpecifier)) {
        const module = statement.moduleSpecifier.text
        const clause = statement.importClause
        if (!clause) continue
        if (clause.name) this.imports.set(clause.name.text, { module, exported: 'default' })
        if (clause.namedBindings && ts.isNamedImports(clause.namedBindings)) {
          for (const element of clause.namedBindings.elements) {
            this.imports.set(element.name.text, {
              module,
              exported: element.propertyName?.text ?? element.name.text
            })
          }
        } else if (clause.namedBindings && ts.isNamespaceImport(clause.namedBindings)) {
          this.imports.set(clause.namedBindings.name.text, { module, exported: '*' })
        }
      }
    }

    const visit = (node: ts.Node): void => {
      if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer) {
        this.aliases.set(node.name.text, node.initializer)
      }
      ts.forEachChild(node, visit)
    }
    visit(this.sourceFile)
  }

  resolveExpression(expression: ts.Expression, seen = new Set<string>()): SemanticSymbol | null {
    if (ts.isParenthesizedExpression(expression))
      return this.resolveExpression(expression.expression, seen)
    if (ts.isIdentifier(expression)) {
      const imported = this.imports.get(expression.text)
      if (imported) return imported
      if (seen.has(expression.text)) return null
      const alias = this.aliases.get(expression.text)
      if (!alias) return { module: '', exported: expression.text }
      seen.add(expression.text)
      return this.resolveExpression(alias, seen)
    }
    if (ts.isPropertyAccessExpression(expression)) {
      const base = this.resolveExpression(expression.expression, seen)
      return { module: base?.module ?? '', exported: expression.name.text }
    }
    if (
      ts.isElementAccessExpression(expression) &&
      expression.argumentExpression &&
      ts.isStringLiteral(expression.argumentExpression)
    ) {
      const base = this.resolveExpression(expression.expression, seen)
      return { module: base?.module ?? '', exported: expression.argumentExpression.text }
    }
    return null
  }

  calls(): ts.CallExpression[] {
    const calls: ts.CallExpression[] = []
    const visit = (node: ts.Node): void => {
      if (ts.isCallExpression(node)) calls.push(node)
      ts.forEachChild(node, visit)
    }
    visit(this.sourceFile)
    return calls
  }

  jsxTags(): Array<{ node: ts.JsxOpeningLikeElement; symbol: SemanticSymbol | null }> {
    const tags: Array<{ node: ts.JsxOpeningLikeElement; symbol: SemanticSymbol | null }> = []
    const visit = (node: ts.Node): void => {
      if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
        const expression = ts.isIdentifier(node.tagName)
          ? node.tagName
          : ts.isPropertyAccessExpression(node.tagName)
            ? node.tagName
            : null
        tags.push({ node, symbol: expression ? this.resolveExpression(expression) : null })
      }
      ts.forEachChild(node, visit)
    }
    visit(this.sourceFile)
    return tags
  }

  hasImport(module: string, exported: string): boolean {
    return [...this.imports.values()].some(
      (item) => item.module === module && item.exported === exported
    )
  }

  hasCall(exported: string, module?: string): boolean {
    return this.calls().some((call) => {
      const symbol = this.resolveExpression(call.expression)
      return symbol?.exported === exported && (module === undefined || symbol.module === module)
    })
  }

  hasJsx(exported: string, module?: string): boolean {
    return this.jsxTags().some(
      ({ symbol }) =>
        symbol?.exported === exported && (module === undefined || symbol.module === module)
    )
  }
}

function findModel(models: TypeScriptModel[], suffix: string): TypeScriptModel | undefined {
  return models.find((model) => normalizePath(model.file).endsWith(suffix))
}

function checkRootParity(findings: GuardFinding[], models: TypeScriptModel[]): void {
  const roots = ['src/renderer/App.tsx', 'src/renderer/TauriApp.tsx'] as const
  for (const suffix of roots) {
    const model = findModel(models, suffix)
    if (!model) {
      findings.push({
        rule: 'source-discovery',
        file: suffix,
        line: 1,
        message: 'renderer root is missing'
      })
      continue
    }
    const requirements: Array<[boolean, string]> = [
      [
        model.hasImport('@/app/PortableAppEffects', 'PortableAppEffects'),
        'renderer root must import PortableAppEffects from the shared portable effects module'
      ],
      [
        model.hasImport('@/app/portable-router', 'createPortableRouter'),
        'renderer root must import createPortableRouter from the shared portable router module'
      ],
      [
        model.hasCall('createPortableRouter', '@/app/portable-router'),
        'renderer root must call the imported createPortableRouter (aliases are supported)'
      ],
      [
        model.hasJsx('PortableAppEffects', '@/app/PortableAppEffects'),
        'renderer root must render the imported PortableAppEffects component'
      ],
      [model.hasJsx('ConversationHostStatus'), 'renderer root must render ConversationHostStatus'],
      [
        model.hasJsx('ConversationRecoveryPanel'),
        'renderer root must render ConversationRecoveryPanel'
      ]
    ]
    for (const [passed, message] of requirements) {
      if (!passed) findings.push({ rule: 'root-parity', file: model.file, line: 1, message })
    }
    for (const call of model.calls()) {
      if (model.resolveExpression(call.expression)?.exported === 'createHashRouter') {
        addFinding(
          findings,
          'root-parity',
          model.file,
          model.sourceFile,
          call,
          'renderer root must not redeclare the portable route table'
        )
      }
      const symbol = model.resolveExpression(call.expression)
      if (
        symbol &&
        PORTABLE_EFFECT_HOOKS.includes(symbol.exported as (typeof PORTABLE_EFFECT_HOOKS)[number])
      ) {
        addFinding(
          findings,
          'root-parity',
          model.file,
          model.sourceFile,
          call,
          `renderer root must not duplicate portable effect hook ${symbol.exported}`
        )
      }
    }
  }

  const effects = findModel(models, 'src/renderer/app/PortableAppEffects.tsx')
  if (!effects) {
    findings.push({
      rule: 'source-discovery',
      file: 'src/renderer/app/PortableAppEffects.tsx',
      line: 1,
      message: 'shared portable effects source is missing'
    })
  } else {
    for (const hook of PORTABLE_EFFECT_HOOKS) {
      if (!effects.hasCall(hook)) {
        findings.push({
          rule: 'root-parity',
          file: effects.file,
          line: 1,
          message: `shared portable effects are missing executable hook call ${hook}`
        })
      }
    }
    if (!effects.hasCall('initNotificationPermissions')) {
      findings.push({
        rule: 'root-parity',
        file: effects.file,
        line: 1,
        message: 'shared portable effects are missing initNotificationPermissions call'
      })
    }
  }

  const router = findModel(models, 'src/renderer/app/portable-router.tsx')
  if (!router) {
    findings.push({
      rule: 'source-discovery',
      file: 'src/renderer/app/portable-router.tsx',
      line: 1,
      message: 'shared portable router source is missing'
    })
  } else {
    const routes = new Set<string>()
    const visit = (node: ts.Node): void => {
      if (ts.isPropertyAssignment(node)) {
        const name =
          ts.isIdentifier(node.name) || ts.isStringLiteral(node.name) ? node.name.text : ''
        if (name === 'path' && ts.isStringLiteralLike(node.initializer))
          routes.add(node.initializer.text)
      }
      ts.forEachChild(node, visit)
    }
    visit(router.sourceFile)
    for (const route of PORTABLE_ROUTES) {
      if (!routes.has(route)) {
        findings.push({
          rule: 'root-parity',
          file: router.file,
          line: 1,
          message: `shared portable router is missing structural path ${route}`
        })
      }
    }
  }
}

function ownerFunctionName(node: ts.Node): string {
  let owner: ts.Node | undefined = node
  while (owner) {
    if (ts.isFunctionDeclaration(owner) && owner.name) return owner.name.text
    if (
      (ts.isArrowFunction(owner) || ts.isFunctionExpression(owner)) &&
      owner.parent &&
      ts.isVariableDeclaration(owner.parent) &&
      ts.isIdentifier(owner.parent.name)
    ) {
      return owner.parent.name.text
    }
    owner = owner.parent
  }
  return ''
}

function checkNavigationAndLegacy(findings: GuardFinding[], models: TypeScriptModel[]): void {
  const navigationName =
    /(?:navigate|selectProject|switchProject|closeTerminalView|PortableAppEffects|App|TauriApp)/i
  const forbiddenTeardown = new Set([
    'terminate',
    'kill',
    'forceKill',
    'kill_all',
    'terminateTerminalResource'
  ])
  const legacyWrites = new Set([
    'writeManifest',
    'deleteManifest',
    'saveHistorySession',
    'deleteHistorySession'
  ])

  for (const model of models.filter((item) => item.file.startsWith('src/renderer/'))) {
    const calls = model.calls()
    const calledByOwner = new Map<string, Set<string>>()
    for (const call of calls) {
      const owner = ownerFunctionName(call)
      const symbol = model.resolveExpression(call.expression)
      const calledName =
        symbol?.exported ?? (ts.isIdentifier(call.expression) ? call.expression.text : '')
      if (!owner || !calledName) continue
      const called = calledByOwner.get(owner) ?? new Set<string>()
      called.add(calledName)
      calledByOwner.set(owner, called)
    }

    for (const call of calls) {
      const symbol = model.resolveExpression(call.expression)
      if (!symbol) continue
      if (
        legacyWrites.has(symbol.exported) &&
        /(?:conversation-store|session-workspace-sync)/.test(model.file)
      ) {
        addFinding(
          findings,
          'legacy-read-only',
          model.file,
          model.sourceFile,
          call,
          'Conversation-first renderer paths must not mutate legacy stores'
        )
      }
      if (!forbiddenTeardown.has(symbol.exported)) continue
      const ownerName = ownerFunctionName(call)
      const calledFromNavigation = [...calledByOwner.entries()].some(
        ([caller, callees]) => navigationName.test(caller) && callees.has(ownerName)
      )
      const rootFile = /(?:^|\/)(?:App|TauriApp)\.tsx$/.test(model.file)
      if (rootFile || navigationName.test(ownerName) || calledFromNavigation) {
        addFinding(
          findings,
          'navigation-preserves-pty',
          model.file,
          model.sourceFile,
          call,
          `navigation helper ${ownerName || '<module>'} must not call ${symbol.exported}`
        )
      }
    }
  }
}

function interfaceMembers(model: TypeScriptModel, name: string): Set<string> {
  const members = new Set<string>()
  for (const statement of model.sourceFile.statements) {
    if (!ts.isInterfaceDeclaration(statement) || statement.name.text !== name) continue
    for (const member of statement.members) {
      if ('name' in member && member.name) {
        if (ts.isIdentifier(member.name) || ts.isStringLiteral(member.name))
          members.add(member.name.text)
      }
    }
  }
  return members
}

function checkTypeContracts(findings: GuardFinding[], models: TypeScriptModel[]): void {
  const workspace = findModel(models, 'src/shared/types/session-workspace.types.ts')
  if (workspace) {
    const members = interfaceMembers(workspace, 'SessionWorkspaceV1')
    for (const field of ['projectId', 'sessionId']) {
      if (members.has(field)) {
        findings.push({
          rule: 'workspace-identity',
          file: workspace.file,
          line: 1,
          message: `SessionWorkspaceV1 must not persist ${field}`
        })
      }
    }
    for (const field of ['claim', 'rawClaim', 'env', 'envVars', 'credentials', 'terminalOutput']) {
      if (members.has(field)) {
        findings.push({
          rule: 'raw-claim',
          file: workspace.file,
          line: 1,
          message: `SessionWorkspaceV1 must not persist secret-bearing field ${field}`
        })
      }
    }
  }

  const terminal = findModel(models, 'src/shared/types/web-terminal-protocol.types.ts')
  if (terminal) {
    const members = interfaceMembers(terminal, 'TerminalSpawnIntentV1')
    for (const field of ['program', 'args', 'env', 'cwd', 'shell']) {
      if (members.has(field)) {
        findings.push({
          rule: 'remote-terminal-intent',
          file: terminal.file,
          line: 1,
          message: `remote TerminalSpawnIntentV1 must not expose caller-controlled ${field}`
        })
      }
    }
  }
}

function checkFacades(findings: GuardFinding[], models: TypeScriptModel[]): void {
  for (const suffix of SHARED_PARSER_ADAPTER_SUFFIXES) {
    const model = findModel(models, suffix)
    if (!model) continue
    const parserImported =
      model.hasImport('@shared/types/conversation.types', 'isConversationId') ||
      model.hasImport('@shared/types/conversation.types', 'parseConversationId')
    const parserCalled = model.hasCall('isConversationId') || model.hasCall('parseConversationId')
    if (!parserImported || !parserCalled) {
      findings.push({
        rule: 'shared-conversation-id-parser',
        file: model.file,
        line: 1,
        message:
          'renderer Conversation adapter must import and call the shared ConversationId parser'
      })
    }
  }

  const facade = findModel(models, 'src/renderer/lib/conversation-api.ts')
  if (facade) {
    for (const forbidden of ['invoke', 'fetch', 'WebSocket']) {
      for (const call of facade.calls()) {
        if (facade.resolveExpression(call.expression)?.exported === forbidden) {
          addFinding(
            findings,
            'facade-transport-ownership',
            facade.file,
            facade.sourceFile,
            call,
            `compatibility Conversation facade must not own ${forbidden} transport logic`
          )
        }
      }
    }
    for (const delegate of [
      'sessionWorkspaceApi',
      'conversationLifecycleApi',
      'tauriConversationApi',
      'webConversationApi'
    ]) {
      if (
        ![...facade.sourceFile.statements].some((statement) => {
          if (!ts.isImportDeclaration(statement) || !statement.importClause?.namedBindings)
            return false
          if (!ts.isNamedImports(statement.importClause.namedBindings)) return false
          return statement.importClause.namedBindings.elements.some(
            (element) => (element.propertyName?.text ?? element.name.text) === delegate
          )
        })
      ) {
        findings.push({
          rule: 'facade-transport-ownership',
          file: facade.file,
          line: 1,
          message: `compatibility facade must import production delegate ${delegate}`
        })
      }
    }
  }

  const history = findModel(models, 'src/renderer/lib/acp-history-persistence.ts')
  if (history) {
    if (
      !history.hasImport('@/lib/acp-history-api', 'acpHistoryApi') ||
      !history.hasCall('getPage')
    ) {
      findings.push({
        rule: 'history-paging-facade',
        file: history.file,
        line: 1,
        message: 'desktop history paging must call acpHistoryApi.getPage through the real facade'
      })
    }
    if (!history.hasCall('getSessionPayloadPage')) {
      findings.push({
        rule: 'history-paging-facade',
        file: history.file,
        line: 1,
        message: 'server history paging must call transport.getSessionPayloadPage'
      })
    }
  }
}

function checkRendererCredential(findings: GuardFinding[], models: TypeScriptModel[]): void {
  const transport = findModel(models, 'src/renderer/lib/acp-transport.ts')
  if (!transport) return
  if (!transport.hasCall('getRemoteAccessCredential')) {
    findings.push({
      rule: 'authenticated-remote-access',
      file: transport.file,
      line: 1,
      message: 'renderer WebSocket transport must call the in-memory credential boundary'
    })
  }
  const visit = (node: ts.Node): void => {
    if (ts.isPropertyAssignment(node)) {
      const name = ts.isIdentifier(node.name) || ts.isStringLiteral(node.name) ? node.name.text : ''
      if (
        (name === 'token' || name === 'credential') &&
        ts.isStringLiteralLike(node.initializer) &&
        ['dev', 'placeholder', 'changeme'].includes(node.initializer.text.toLowerCase())
      ) {
        addFinding(
          findings,
          'authenticated-remote-access',
          transport.file,
          transport.sourceFile,
          node,
          'placeholder remote authentication credential is forbidden'
        )
      }
    }
    ts.forEachChild(node, visit)
  }
  visit(transport.sourceFile)
}

interface WorkflowRun {
  command: string
  line: number
  job: string
  step: string
}

function workflowRuns(file: string, source: string, findings: GuardFinding[]): WorkflowRun[] {
  const counter = new LineCounter()
  const document = parseDocument(source, { lineCounter: counter })
  if (document.errors.length > 0) {
    findings.push({
      rule: 'workflow-yaml',
      file,
      line: 1,
      message: `workflow YAML parse failed: ${document.errors[0]?.message ?? 'unknown parse error'}`
    })
    return []
  }
  const runs: WorkflowRun[] = []
  const walk = (node: YamlNode | null | undefined, path: string[]): void => {
    if (!node) return
    if (isMap(node)) {
      for (const pair of node.items) {
        const key = isScalar(pair.key) ? String(pair.key.value) : '<key>'
        if (key === 'run' && isScalar(pair.value) && typeof pair.value.value === 'string') {
          const position = pair.value.range
            ? counter.linePos(pair.value.range[0])
            : { line: 1, col: 1 }
          const jobIndex = path.indexOf('jobs')
          const stepsIndex = path.lastIndexOf('steps')
          runs.push({
            command: pair.value.value,
            line: position.line,
            job: jobIndex >= 0 ? (path[jobIndex + 1] ?? '<job>') : '<job>',
            step: stepsIndex >= 0 ? (path[stepsIndex + 1] ?? '<step>') : '<step>'
          })
        }
        walk(pair.value as YamlNode | null, [...path, key])
      }
    } else if (isSeq(node)) {
      for (const [index, item] of node.items.entries()) {
        walk(item as YamlNode | null, [...path, String(index)])
      }
    }
  }
  walk(document.contents as YamlNode | null, [])
  return runs
}

function cargoCommandSegments(command: string): string[] {
  const joined = command.replace(/\\\r?\n/g, ' ')
  return joined
    .split(/\r?\n|&&|;/)
    .map((segment) => segment.trim())
    .filter(Boolean)
}

function checkWorkflows(findings: GuardFinding[], sources: GuardSources): void {
  const workflows = Object.entries(sources).filter(([file]) =>
    /^\.github\/workflows\/.*\.(?:yml|yaml)$/.test(normalizePath(file))
  )
  if (workflows.length === 0) {
    findings.push({
      rule: 'source-discovery',
      file: '.github/workflows',
      line: 1,
      message: 'no active workflow YAML files were discovered'
    })
    return
  }

  for (const [file, source] of workflows) {
    for (const run of workflowRuns(file, source, findings)) {
      for (const segment of cargoCommandSegments(run.command)) {
        const match = segment.match(/\bcargo\s+(metadata|check|test|clippy|build)\b/)
        if (!match || /(?:^|\s)--locked(?:\s|$)/.test(segment)) continue
        findings.push({
          rule: 'locked-rust-ci',
          file,
          line: run.line,
          message: `job=${run.job} step=${run.step} cargo ${match[1]} command must use --locked`
        })
      }
    }
  }

  const validation = workflows.find(([file]) => file.endsWith('/pr-validation.yml'))
  if (!validation) {
    findings.push({
      rule: 'native-ci-wiring',
      file: '.github/workflows/pr-validation.yml',
      line: 1,
      message: 'PR validation workflow is missing'
    })
    return
  }
  const [file, source] = validation
  const document = parseDocument(source)
  const data = document.toJS() as {
    jobs?: Record<string, { strategy?: { matrix?: unknown }; steps?: Array<{ run?: string }> }>
  }
  const jobs = data.jobs ?? {}
  const durability = jobs['conversation-native-durability']
  const durabilityJson = JSON.stringify(durability?.strategy?.matrix ?? {})
  for (const platform of ['linux', 'macos', 'windows']) {
    if (!durabilityJson.includes(platform)) {
      findings.push({
        rule: 'native-ci-wiring',
        file,
        line: 1,
        message: `locked native durability matrix is missing ${platform}`
      })
    }
  }
  const commands = Object.values(jobs)
    .flatMap((job) => job.steps ?? [])
    .map((step) => step.run ?? '')
  for (const required of [
    'cargo test --locked conversation::native_durability_tests',
    'cargo test --locked --test conversation_first_guardrails',
    'cargo build --locked --bin termul-server --features standalone-server',
    'cargo clippy --locked --bin termul-server --features standalone-server -- -D warnings'
  ]) {
    if (!commands.some((command) => command.includes(required))) {
      findings.push({
        rule: 'native-ci-wiring',
        file,
        line: 1,
        message: `locked native/semantic CI wiring is missing ${required}`
      })
    }
  }
}

/** Compatibility helper retained for callers; semantic checks use compiler AST nodes. */
export function stripComments(source: string): string {
  const scanner = ts.createScanner(
    ts.ScriptTarget.Latest,
    false,
    ts.LanguageVariant.Standard,
    source
  )
  let result = ''
  let position = 0
  for (let token = scanner.scan(); token !== ts.SyntaxKind.EndOfFileToken; token = scanner.scan()) {
    const start = scanner.getTokenPos()
    const end = scanner.getTextPos()
    result += source.slice(position, start)
    if (
      token === ts.SyntaxKind.SingleLineCommentTrivia ||
      token === ts.SyntaxKind.MultiLineCommentTrivia
    ) {
      result += source.slice(start, end).replace(/[^\n]/g, ' ')
    } else {
      result += source.slice(start, end)
    }
    position = end
  }
  return result + source.slice(position)
}

/** Compatibility helper retained for legacy tests; Rust enforcement lives in the syn guard. */
export function stripRustTestCode(source: string): string {
  return source.replace(/#\[cfg\(test\)\][\s\S]*$/m, (matched) => matched.replace(/[^\n]/g, ' '))
}

export function checkConversationFirstGuardrails(sources: GuardSources): GuardFinding[] {
  const models = Object.entries(sources)
    .filter(([file]) => isTypeScriptFile(file))
    .map(([file, source]) => new TypeScriptModel(normalizePath(file), source))
  const findings: GuardFinding[] = []
  checkRootParity(findings, models)
  checkNavigationAndLegacy(findings, models)
  checkTypeContracts(findings, models)
  checkFacades(findings, models)
  checkRendererCredential(findings, models)
  checkWorkflows(findings, sources)
  return findings.sort(
    (left, right) =>
      left.file.localeCompare(right.file) ||
      left.line - right.line ||
      left.rule.localeCompare(right.rule) ||
      left.message.localeCompare(right.message)
  )
}

function walkFiles(root: string, directory: string, accept: (path: string) => boolean): string[] {
  const absolute = resolve(root, directory)
  const files: string[] = []
  for (const entry of readdirSync(absolute, { withFileTypes: true })) {
    const path = join(absolute, entry.name)
    if (entry.isDirectory())
      files.push(...walkFiles(root, normalizePath(relative(root, path)), accept))
    else if (entry.isFile() && accept(path)) files.push(normalizePath(relative(root, path)))
  }
  return files
}

export function loadRepositorySources(root = process.cwd()): GuardSources {
  const sourceFiles = [
    ...walkFiles(
      root,
      'src/renderer',
      (path) => ['.ts', '.tsx'].includes(extname(path)) && !path.endsWith('.d.ts')
    ),
    ...walkFiles(
      root,
      'src/shared',
      (path) => ['.ts', '.tsx'].includes(extname(path)) && !path.endsWith('.d.ts')
    ),
    ...walkFiles(root, '.github/workflows', (path) => ['.yml', '.yaml'].includes(extname(path)))
  ].sort()
  return Object.fromEntries(
    sourceFiles.map((file) => [file, readFileSync(resolve(root, file), 'utf8')])
  )
}

export function main(sources: GuardSources = loadRepositorySources()): number {
  const findings = checkConversationFirstGuardrails(sources)
  if (findings.length === 0) {
    console.log(
      `Conversation-first semantic guardrails passed (${Object.keys(sources).length} discovered sources)`
    )
    return 0
  }
  for (const item of findings) {
    console.error(`${item.file}:${item.line} [${item.rule}] ${item.message}`)
  }
  console.error(`Conversation-first semantic guardrails failed with ${findings.length} finding(s)`)
  return 1
}

if (import.meta.main) process.exit(main())
