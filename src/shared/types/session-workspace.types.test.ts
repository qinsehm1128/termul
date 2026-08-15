import { describe, expect, expectTypeOf, it } from 'vitest'
import type { ResolveRecoveryItemRequest } from './conversation-recovery.types'
import {
  SESSION_WORKSPACE_SCHEMA_VERSION,
  type SessionWorkspaceLoadOutcome,
  type SessionWorkspaceV1,
  type SessionWorkspaceWriteOutcome,
  type TerminalResourceDescriptor
} from './session-workspace.types'

const conversationId = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'

function workspace(revision = 4): SessionWorkspaceV1 {
  return {
    schemaVersion: SESSION_WORKSPACE_SCHEMA_VERSION,
    conversationId,
    revision,
    updatedAtUtc: '2026-08-15T10:00:00.000Z',
    updateIdentity: 'renderer-one',
    topology: {
      type: 'leaf',
      id: 'leaf-one',
      terminalIds: ['terminal-one'],
      editorIds: ['edit-/src/main.ts'],
      activeTabId: 'term-terminal-one'
    },
    activePaneId: 'leaf-one',
    resources: [
      { kind: 'terminal', terminalId: 'terminal-one', conversationId },
      { kind: 'editor', editorId: 'edit-/src/main.ts', filePath: '/src/main.ts' }
    ],
    projectionState: { status: 'native' }
  }
}

describe('SessionWorkspace contract', () => {
  it('pins ConversationId identity, passive resources, and host revision fields', () => {
    const value = workspace()
    expect(value.conversationId).toBe(conversationId)
    expect(value.revision).toBe(4)
    expect(value.updatedAtUtc).toMatch(/\.000Z$/)
    expect(value.resources).toEqual([
      { kind: 'terminal', terminalId: 'terminal-one', conversationId },
      { kind: 'editor', editorId: 'edit-/src/main.ts', filePath: '/src/main.ts' }
    ])
    expect(JSON.stringify(value)).not.toMatch(/claim|envVars|credentials|terminalOutput|viewport/i)
  })

  it('pins load, conflict, and recovery discriminators', () => {
    const outcomes: SessionWorkspaceLoadOutcome[] = [
      { status: 'missing', conversationId },
      { status: 'loaded', workspace: workspace() },
      { status: 'recoveryRequired', conversationId, recoveryItems: [] }
    ]
    const writes: SessionWorkspaceWriteOutcome[] = [
      { status: 'updated', revision: 5, updatedAtUtc: '2026-08-15T10:00:01.000Z' },
      {
        status: 'conflict',
        currentRevision: 5,
        currentUpdatedAtUtc: '2026-08-15T10:00:01.000Z',
        currentUpdateIdentity: 'renderer-two'
      },
      { status: 'recoveryRequired', recoveryItems: [] }
    ]
    expect(outcomes.map((outcome) => outcome.status)).toEqual([
      'missing',
      'loaded',
      'recoveryRequired'
    ])
    expect(writes.map((outcome) => outcome.status)).toEqual([
      'updated',
      'conflict',
      'recoveryRequired'
    ])
  })

  it('imports the exact shared recovery request contract', () => {
    expectTypeOf<ResolveRecoveryItemRequest['action']>().toEqualTypeOf<
      'inspect' | 'associateConversation' | 'startEmptyWorkspace' | 'dismissPreservedSource'
    >()
    expectTypeOf<TerminalResourceDescriptor>().not.toHaveProperty('claim')
    expectTypeOf<TerminalResourceDescriptor>().not.toHaveProperty('envVars')
    expectTypeOf<TerminalResourceDescriptor>().not.toHaveProperty('credentials')
  })
})
