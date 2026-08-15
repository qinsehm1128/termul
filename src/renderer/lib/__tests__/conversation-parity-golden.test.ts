import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import {
  RECOVERY_ACTION_FIXTURES,
  type ResolveRecoveryItemRequest
} from '@shared/types/conversation-recovery.types'
import { invoke } from '@tauri-apps/api/core'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  _resetAcpTransportForTests,
  _setAcpTransportForTests,
  type AcpTransport,
  AcpTransportError
} from '@/lib/acp-transport'
import { createTauriConversationApi } from '@/lib/tauri-conversation-api'
import { createWebConversationApi } from '@/lib/web-conversation-api'

const ID = '018f7a1c-1b4d-7c8a-9f01-0123456789ab'

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' }
  })
}

beforeEach(() => {
  vi.clearAllMocks()
})

afterEach(() => {
  _resetAcpTransportForTests()
  vi.unstubAllGlobals()
})

describe('Conversation transport golden parity', () => {
  it('pins Tauri list/open/legacy request names, camelCase payloads, and stable failures', async () => {
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === 'conversation_resolve_legacy_id') {
        expect(args).toEqual({
          request: { sourceKind: 'legacyStorageKey', value: 'legacy-one' }
        })
        return {
          success: true,
          data: { conversationId: ID, canonicalRoute: `#/c/${ID}` }
        }
      }
      if (command === 'conversation_open') {
        expect(args).toEqual({ conversationId: ID })
        return {
          success: false,
          code: 'CONVERSATION_RECOVERY_REQUIRED',
          error: 'recovery required'
        }
      }
      return { success: true, data: [] }
    })

    const api = createTauriConversationApi()
    await expect(api.listConversations()).resolves.toEqual({ success: true, data: [] })
    await expect(
      api.resolveLegacyConversationId({ sourceKind: 'legacyStorageKey', value: 'legacy-one' })
    ).resolves.toEqual({
      success: true,
      data: { conversationId: ID, canonicalRoute: `#/c/${ID}` }
    })
    await expect(api.openConversation(ID)).resolves.toEqual({
      success: false,
      code: 'CONVERSATION_RECOVERY_REQUIRED',
      error: 'recovery required'
    })
  })

  it('uses same-origin HTTP for reads and explicit FORBIDDEN for denied mutations', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url.endsWith('/conversations/resolve-legacy')) {
        expect(init?.body).toBe(
          JSON.stringify({ sourceKind: 'legacyChatHistoryId', value: 'history-one' })
        )
        return response({
          success: true,
          data: { conversationId: ID, canonicalRoute: `#/c/${ID}` }
        })
      }
      return response({ success: false, code: 'FORBIDDEN', error: 'localhost-only' })
    })
    vi.stubGlobal('fetch', fetchMock)

    const api = createWebConversationApi()
    await expect(
      api.resolveLegacyConversationId({
        sourceKind: 'legacyChatHistoryId',
        value: 'history-one'
      })
    ).resolves.toEqual({
      success: true,
      data: { conversationId: ID, canonicalRoute: `#/c/${ID}` }
    })
    await expect(
      api.writeWorkspace(ID, null, {
        schemaVersion: 1,
        conversationId: ID,
        revision: 0,
        updatedAtUtc: '',
        resources: [],
        projectionState: { status: 'native' }
      })
    ).resolves.toEqual({ success: false, code: 'FORBIDDEN', error: 'localhost-only' })
  })

  it('uses authenticated WS for remote recovery mutations and preserves application codes', async () => {
    vi.spyOn(window, 'location', 'get').mockReturnValue({
      hostname: 'phone.example',
      origin: 'https://phone.example'
    } as Location)
    const request = RECOVERY_ACTION_FIXTURES[1].request as ResolveRecoveryItemRequest
    const conversationRequest = vi.fn(async (type: string, payload: unknown) => {
      expect(type).toBe('resolve_recovery_item')
      expect(payload).toEqual(request)
      return RECOVERY_ACTION_FIXTURES[1].result
    })
    _setAcpTransportForTests({
      conversationRequest,
      dispose: vi.fn()
    } as unknown as AcpTransport)

    const api = createWebConversationApi()
    await expect(api.resolveRecovery(request)).resolves.toEqual({
      success: true,
      data: RECOVERY_ACTION_FIXTURES[1].result
    })
    expect(conversationRequest).toHaveBeenCalledTimes(1)

    conversationRequest.mockRejectedValueOnce(
      new AcpTransportError('LEGACY_COMPATIBILITY_READ_ONLY', 'legacy source is read-only')
    )
    await expect(api.resolveRecovery(request)).resolves.toEqual({
      success: false,
      code: 'LEGACY_COMPATIBILITY_READ_ONLY',
      error: 'legacy source is read-only'
    })
  })

  it('reuses the authoritative RecoveryAction union without local action aliases', () => {
    const root = join(__dirname, '..', '..', '..', '..')
    const files = [
      'src/shared/types/conversation-api.types.ts',
      'src/renderer/lib/tauri-conversation-api.ts',
      'src/renderer/lib/web-conversation-api.ts',
      'src/renderer/hooks/use-conversation-host-bootstrap.ts',
      'src/renderer/components/conversation/ConversationHostStatus.tsx'
    ]
    for (const relative of files) {
      const source = readFileSync(join(root, relative), 'utf8')
      expect(source, relative).not.toMatch(
        /'inspect'\s*\|\s*'associateConversation'|"inspect"\s*\|\s*"associateConversation"/
      )
    }
    const contract = readFileSync(join(root, 'src/shared/types/conversation-api.types.ts'), 'utf8')
    expect(contract).toContain("from './conversation-recovery.types'")
    expect(contract).toContain('export type {')
  })
})
