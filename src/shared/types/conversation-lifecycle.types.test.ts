import { describe, expect, it } from 'vitest'
import {
  CONVERSATION_LIFECYCLE_ACTIONS,
  CONVERSATION_LIFECYCLE_ERROR_CODES,
  type ConversationLifecycleOutcome
} from './conversation-lifecycle.types'

const conversationId = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'

describe('Conversation lifecycle wire contract', () => {
  it('pins camelCase lifecycle action discriminators', () => {
    expect(CONVERSATION_LIFECYCLE_ACTIONS).toEqual([
      'detachBinding',
      'rebindDetachedBinding',
      'suspendBinding',
      'replaceBinding',
      'deleteConversation'
    ])
  })

  it('pins stable conflict, blocker, provider, and recovery codes', () => {
    expect(CONVERSATION_LIFECYCLE_ERROR_CODES).toContain('CONVERSATION_CONFLICT')
    expect(CONVERSATION_LIFECYCLE_ERROR_CODES).toContain('CONVERSATION_LIVE_RESOURCES')
    expect(CONVERSATION_LIFECYCLE_ERROR_CODES).toContain('ACP_CLOSE_UNSUPPORTED')
    expect(CONVERSATION_LIFECYCLE_ERROR_CODES).toContain('CONVERSATION_RECOVERY_REQUIRED')
  })

  it('distinguishes updated binding state from delete blockers', () => {
    const detached: ConversationLifecycleOutcome = {
      status: 'updated',
      action: 'detachBinding',
      conversationId,
      previousRevision: 4,
      revision: 5,
      workspaceCwd: '/visible/conversation',
      lifecycleState: 'ready',
      currentBinding: {
        schemaVersion: 1,
        bindingId: 'b2832b54-2ca4-4db4-93fd-f93bf6793114',
        agentSessionId: 'opaque/session',
        runtimeAgentId: 'agent-runtime',
        stableAgentNamespace: 'config:test',
        executionCwd: '/visible/conversation',
        boundAtUtc: '2026-08-15T09:45:16.000Z',
        state: 'detached'
      }
    }
    const blocked: ConversationLifecycleOutcome = {
      status: 'blocked',
      action: 'deleteConversation',
      conversationId,
      revision: 5,
      code: 'CONVERSATION_LIVE_RESOURCES',
      blockers: [
        { kind: 'liveBinding', count: 1, ids: ['opaque/session'] },
        { kind: 'terminalResources', count: 2, ids: ['terminal-1', 'terminal-2'] }
      ]
    }
    expect(detached.currentBinding?.state).toBe('detached')
    expect(blocked.blockers.map((blocker) => blocker.kind)).toEqual([
      'liveBinding',
      'terminalResources'
    ])
  })
})
