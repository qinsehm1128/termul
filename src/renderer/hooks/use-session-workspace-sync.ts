import type { ConversationId } from '@shared/types/conversation.types'
import type {
  RecoveryActionName,
  RecoveryItemV1,
  ResolveRecoveryItemRequest
} from '@shared/types/conversation-recovery.types'
import type {
  EditorResourceDescriptor,
  SessionWorkspacePaneNode,
  SessionWorkspaceResourceDescriptor,
  SessionWorkspaceV1,
  TerminalResourceDescriptor
} from '@shared/types/session-workspace.types'
import { useEffect } from 'react'
import { isTerminalRestoreInProgress } from '@/hooks/useTerminalAutoSave'
import { logFrontendError } from '@/lib/log-api'
import { sessionWorkspaceApi } from '@/lib/session-workspace-api'
import { randomUUID } from '@/lib/uuid'
import { useAcpStore } from '@/stores/acp-store'
import { useEditorStore } from '@/stores/editor-store'
import { useSessionWorkspaceSyncStore } from '@/stores/session-workspace-sync-store'
import { useTerminalStore } from '@/stores/terminal-store'
import type { WorkspaceTab } from '@/stores/workspace-store'
import { editorTabId, terminalTabId, useWorkspaceStore } from '@/stores/workspace-store'
import type { LeafNode, PaneNode, SplitNode } from '@/types/workspace.types'

const canonicalUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
const WRITE_DEBOUNCE_MS = 500
let updateIdentity: string | null = null

function rendererIdentity(): string {
  updateIdentity ??= `renderer-${randomUUID()}`
  return updateIdentity
}

export function getActiveConversationId(): ConversationId | null {
  const state = useAcpStore.getState()
  if (!state.activeSessionId) return null
  const candidate = state.sessions[state.activeSessionId]?.conversationId
  return candidate && canonicalUuid.test(candidate) ? candidate : null
}

function serializeTopology(
  node: PaneNode,
  conversationId: ConversationId
): SessionWorkspacePaneNode {
  if (node.type === 'split') {
    return {
      type: 'split',
      id: node.id,
      direction: node.direction,
      children: node.children.map((child) => serializeTopology(child, conversationId)),
      sizes: node.sizes
    }
  }
  const terminalIds: string[] = []
  const editorIds: string[] = []
  const terminals = useTerminalStore.getState().terminals
  for (const tab of node.tabs) {
    if (tab.type === 'terminal') {
      const terminal = terminals.find((candidate) => candidate.id === tab.terminalId)
      if (terminal?.conversationId === conversationId) terminalIds.push(tab.terminalId)
    }
    if (tab.type === 'editor') editorIds.push(editorTabId(tab.filePath))
  }
  return {
    type: 'leaf',
    id: node.id,
    terminalIds,
    editorIds,
    activeTabId: node.activeTabId
  }
}

function collectReferencedIds(
  node: SessionWorkspacePaneNode,
  terminals: Set<string>,
  editors: Set<string>
): void {
  if (node.type === 'split') {
    for (const child of node.children) collectReferencedIds(child, terminals, editors)
    return
  }
  for (const id of node.terminalIds) terminals.add(id)
  for (const id of node.editorIds) editors.add(id)
}

export function buildSessionWorkspace(conversationId: ConversationId): SessionWorkspaceV1 {
  const workspaceState = useWorkspaceStore.getState()
  const topology = serializeTopology(workspaceState.root, conversationId)
  const referencedTerminals = new Set<string>()
  const referencedEditors = new Set<string>()
  collectReferencedIds(topology, referencedTerminals, referencedEditors)
  const resources: SessionWorkspaceResourceDescriptor[] = []

  for (const terminal of useTerminalStore.getState().terminals) {
    if (terminal.conversationId !== conversationId || !terminal.ptyId) continue
    const descriptor: TerminalResourceDescriptor = {
      kind: 'terminal',
      terminalId: terminal.ptyId,
      terminalRecordId: terminal.id,
      conversationId
    }
    resources.push(descriptor)
  }

  for (const [filePath] of useEditorStore.getState().openFiles) {
    const editorId = editorTabId(filePath)
    if (!referencedEditors.has(editorId)) continue
    const descriptor: EditorResourceDescriptor = { kind: 'editor', editorId, filePath }
    resources.push(descriptor)
  }

  return {
    schemaVersion: 1,
    conversationId,
    revision: 0,
    updatedAtUtc: '',
    updateIdentity: rendererIdentity(),
    topology,
    activePaneId: workspaceState.activePaneId,
    resources,
    projectionState: { status: 'native' }
  }
}

function rebuildTopology(
  node: SessionWorkspacePaneNode,
  terminals: ReadonlyMap<string, TerminalResourceDescriptor>,
  editors: ReadonlyMap<string, EditorResourceDescriptor>,
  conversationId: ConversationId
): PaneNode {
  if (node.type === 'split') {
    const split: SplitNode = {
      type: 'split',
      id: node.id,
      direction: node.direction,
      children: node.children.map((child) =>
        rebuildTopology(child, terminals, editors, conversationId)
      ),
      sizes: node.sizes
    }
    return split
  }
  const tabs: WorkspaceTab[] = []
  const liveTerminals = useTerminalStore.getState().terminals
  for (const terminalId of node.terminalIds) {
    const descriptor = terminals.get(terminalId)
    const live = liveTerminals.find((terminal) => terminal.id === terminalId)
    const liveConversationId = live?.conversationId
    if (
      !descriptor ||
      descriptor.conversationId !== conversationId ||
      liveConversationId !== conversationId
    ) {
      continue
    }
    tabs.push({ type: 'terminal', id: terminalTabId(terminalId), terminalId })
  }
  for (const editorId of node.editorIds) {
    const descriptor = editors.get(editorId)
    if (!descriptor) continue
    tabs.push({
      type: 'editor',
      id: editorTabId(descriptor.filePath),
      filePath: descriptor.filePath
    })
  }
  const activeTabId =
    node.activeTabId && tabs.some((tab) => tab.id === node.activeTabId)
      ? node.activeTabId
      : (tabs[0]?.id ?? null)
  const leaf: LeafNode = { type: 'leaf', id: node.id, tabs, activeTabId }
  return leaf
}

function firstLeafId(node: PaneNode): string | null {
  if (node.type === 'leaf') return node.id
  for (const child of node.children) {
    const id = firstLeafId(child)
    if (id) return id
  }
  return null
}

function findLeafId(node: PaneNode, id: string): boolean {
  if (node.type === 'leaf') return node.id === id
  return node.children.some((child) => findLeafId(child, id))
}

function loadConversationWorkspace(
  conversationId: ConversationId,
  workspace: SessionWorkspaceV1
): void {
  const terminals = new Map<string, TerminalResourceDescriptor>()
  const editors = new Map<string, EditorResourceDescriptor>()
  for (const resource of workspace.resources) {
    if (resource.kind === 'terminal') {
      terminals.set(resource.terminalRecordId ?? resource.terminalId, resource)
    } else editors.set(resource.editorId, resource)
  }
  let root: PaneNode = workspace.topology
    ? rebuildTopology(workspace.topology, terminals, editors, conversationId)
    : { type: 'leaf', id: randomUUID(), tabs: [], activeTabId: null }
  let firstPaneId = firstLeafId(root)
  if (!firstPaneId) {
    firstPaneId = randomUUID()
    root = { type: 'leaf', id: firstPaneId, tabs: [], activeTabId: null }
  }
  const activePaneId =
    workspace.activePaneId && findLeafId(root, workspace.activePaneId)
      ? workspace.activePaneId
      : firstPaneId
  useWorkspaceStore.setState({ root, activePaneId })
  const editorStore = useEditorStore.getState()
  for (const editor of editors.values()) {
    if (!editorStore.openFiles.has(editor.filePath)) {
      void editorStore.openFile(editor.filePath).catch(() => undefined)
    }
  }
}

export async function loadSessionWorkspace(conversationId: ConversationId): Promise<boolean> {
  const store = useSessionWorkspaceSyncStore.getState()
  store.setRestoreInProgress(conversationId, true)
  try {
    const result = await sessionWorkspaceApi.getWorkspace(conversationId)
    if (!result.success) {
      void logFrontendError({
        source: 'session-workspace-sync',
        message: `workspace load failed code=${result.code} conversationId=${conversationId}`
      })
      return false
    }
    const outcome = result.data
    store.setLoadOutcome(conversationId, outcome)
    if (outcome.status === 'loaded') {
      loadConversationWorkspace(conversationId, outcome.workspace)
      store.setBasedRevision(conversationId, outcome.workspace.revision)
      store.setRecoveryItems(conversationId, [])
      return true
    }
    store.setBasedRevision(conversationId, null)
    store.setRecoveryItems(
      conversationId,
      outcome.status === 'recoveryRequired' ? outcome.recoveryItems : []
    )
    return false
  } finally {
    store.setRestoreInProgress(conversationId, false)
  }
}

export type SessionWorkspaceWriteResult =
  | 'updated'
  | 'conflict'
  | 'recoveryRequired'
  | 'failed'
  | 'skipped'

export async function performSessionWorkspaceWrite(
  conversationId: ConversationId
): Promise<SessionWorkspaceWriteResult> {
  const store = useSessionWorkspaceSyncStore.getState()
  if (
    !conversationId ||
    store.isRestoreInProgress(conversationId) ||
    isTerminalRestoreInProgress() ||
    store.getConflict(conversationId)
  ) {
    return 'skipped'
  }
  const result = await sessionWorkspaceApi.writeWorkspace(
    conversationId,
    store.getBasedRevision(conversationId),
    buildSessionWorkspace(conversationId)
  )
  if (!result.success) {
    void logFrontendError({
      source: 'session-workspace-sync',
      message: `workspace write failed code=${result.code} conversationId=${conversationId}`
    })
    return 'failed'
  }
  if (result.data.status === 'updated') {
    store.setBasedRevision(conversationId, result.data.revision)
    return 'updated'
  }
  if (result.data.status === 'conflict') {
    store.setConflict(conversationId, {
      conversationId,
      currentRevision: result.data.currentRevision,
      currentUpdatedAtUtc: result.data.currentUpdatedAtUtc,
      currentUpdateIdentity: result.data.currentUpdateIdentity
    })
    return 'conflict'
  }
  store.setRecoveryItems(conversationId, result.data.recoveryItems)
  return 'recoveryRequired'
}

export async function resolveSessionWorkspaceConflict(
  conversationId: ConversationId,
  action: 'reload' | 'overwrite' | 'dismiss'
): Promise<void> {
  const store = useSessionWorkspaceSyncStore.getState()
  const conflict = store.getConflict(conversationId)
  if (!conflict) return
  if (action === 'reload') {
    store.setConflict(conversationId, null)
    await loadSessionWorkspace(conversationId)
    return
  }
  store.setBasedRevision(conversationId, conflict.currentRevision)
  store.setConflict(conversationId, null)
  if (action === 'overwrite') {
    const outcome = await performSessionWorkspaceWrite(conversationId)
    if (outcome === 'failed' || outcome === 'skipped') {
      store.setConflict(conversationId, conflict)
    }
  }
}

export async function resolveSessionWorkspaceRecovery(
  conversationId: ConversationId,
  item: RecoveryItemV1,
  action: RecoveryActionName
): Promise<void> {
  const common = { recoveryId: item.recoveryId, expectedRevision: item.revision }
  let request: ResolveRecoveryItemRequest
  switch (action) {
    case 'inspect':
      request = { ...common, action, payload: {} }
      break
    case 'associateConversation':
      request = {
        ...common,
        action,
        idempotencyKey: randomUUID(),
        payload: { conversationId }
      }
      break
    case 'startEmptyWorkspace':
      request = {
        ...common,
        action,
        idempotencyKey: randomUUID(),
        payload: {
          conversationId,
          expectedWorkspaceRevision: useSessionWorkspaceSyncStore
            .getState()
            .getBasedRevision(conversationId)
        }
      }
      break
    case 'dismissPreservedSource':
      request = {
        ...common,
        action,
        idempotencyKey: randomUUID(),
        payload: { reasonCode: 'deferLegacyProjection' }
      }
      break
  }
  const result = await sessionWorkspaceApi.resolveRecovery(request)
  if (!result.success) {
    void logFrontendError({
      source: 'session-workspace-sync',
      message: `recovery action failed code=${result.code} conversationId=${conversationId} recoveryId=${item.recoveryId} action=${action}`
    })
    return
  }
  const updated: RecoveryItemV1 = {
    ...item,
    status: result.data.status,
    revision: result.data.recoveryRevision
  }
  const store = useSessionWorkspaceSyncStore.getState()
  store.setRecoveryItems(
    conversationId,
    store
      .getRecoveryItems(conversationId)
      .map((existing) => (existing.recoveryId === item.recoveryId ? updated : existing))
  )
  if (result.data.workspaceChanged) await loadSessionWorkspace(conversationId)
}

export function useSessionWorkspaceBootstrap(): void {
  const activeSessionId = useAcpStore((state) => state.activeSessionId)
  const sessions = useAcpStore((state) => state.sessions)
  useEffect(() => {
    const candidate = activeSessionId ? sessions[activeSessionId]?.conversationId : undefined
    const conversationId = candidate && canonicalUuid.test(candidate) ? candidate : null
    useSessionWorkspaceSyncStore.getState().setActiveConversationId(conversationId)
    if (conversationId) void loadSessionWorkspace(conversationId)
  }, [activeSessionId, sessions])
}

export function useSessionWorkspaceSync(conversationId: ConversationId | null): void {
  useEffect(() => {
    if (!conversationId) return
    let timer: ReturnType<typeof setTimeout> | null = null
    const schedule = (): void => {
      if (timer) clearTimeout(timer)
      timer = setTimeout(() => {
        timer = null
        void performSessionWorkspaceWrite(conversationId)
      }, WRITE_DEBOUNCE_MS)
    }
    const unsubscribeWorkspace = useWorkspaceStore.subscribe((state, previous) => {
      if (state.root !== previous.root || state.activePaneId !== previous.activePaneId) schedule()
    })
    const unsubscribeEditor = useEditorStore.subscribe((state, previous) => {
      if (state.openFiles !== previous.openFiles) schedule()
    })
    const unsubscribeTerminal = useTerminalStore.subscribe((state, previous) => {
      if (state.terminals !== previous.terminals) schedule()
    })
    return () => {
      if (timer) clearTimeout(timer)
      unsubscribeWorkspace()
      unsubscribeEditor()
      unsubscribeTerminal()
    }
  }, [conversationId])
}
