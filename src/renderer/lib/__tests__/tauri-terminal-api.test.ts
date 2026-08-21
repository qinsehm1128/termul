import { beforeEach, describe, expect, it, vi } from 'vitest'

const mockInvoke = vi.fn()
const mockListen = vi.fn()

vi.mock('@tauri-apps/api/core', () => {
  // Minimal Tauri binary Channel double: records the `onmessage` handler so
  // tests can drive streamed bytes exactly like the Rust forwarders do.
  class Channel<TMessage = ArrayBuffer> {
    onmessage: ((message: TMessage) => void) | null = null
  }
  return { invoke: mockInvoke, Channel }
})

vi.mock('@tauri-apps/api/event', () => ({
  listen: mockListen
}))

describe('tauri-terminal-api', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
  })

  it('shares one Tauri listener across multiple onExit subscribers and tears down after last unsubscribe', async () => {
    ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}

    const unlisten = vi.fn()

    mockListen.mockResolvedValue(unlisten)

    const { createTauriTerminalApi } = await import('../tauri-terminal-api')
    const api = createTauriTerminalApi()

    const callbackA = vi.fn()
    const callbackB = vi.fn()

    const unsubscribeA = api.onExit(callbackA)
    const unsubscribeB = api.onExit(callbackB)

    await Promise.resolve()

    expect(mockListen).toHaveBeenCalledTimes(1)

    unsubscribeA()
    expect(unlisten).not.toHaveBeenCalled()

    unsubscribeB()
    await Promise.resolve()

    expect(unlisten).toHaveBeenCalledTimes(1)
  })

  it('registers new native listener after previous shared listener fully unsubscribed', async () => {
    ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}

    const unlistenA = vi.fn()
    const unlistenB = vi.fn()

    mockListen.mockResolvedValueOnce(unlistenA).mockResolvedValueOnce(unlistenB)

    const { createTauriTerminalApi } = await import('../tauri-terminal-api')
    const api = createTauriTerminalApi()

    const unsubscribeA = api.onExit(vi.fn())
    await Promise.resolve()
    expect(mockListen).toHaveBeenCalledTimes(1)

    unsubscribeA()
    await Promise.resolve()
    expect(unlistenA).toHaveBeenCalledTimes(1)

    const unsubscribeB = api.onExit(vi.fn())
    await Promise.resolve()
    expect(mockListen).toHaveBeenCalledTimes(2)

    unsubscribeB()
    await Promise.resolve()
    expect(unlistenB).toHaveBeenCalledTimes(1)
  })

  it('skips native listener registration outside Tauri context', async () => {
    const { createTauriTerminalApi } = await import('../tauri-terminal-api')
    const api = createTauriTerminalApi()

    const unsubscribe = api.onExit(vi.fn())
    unsubscribe()

    expect(mockListen).not.toHaveBeenCalled()
  })

  describe('CAP-3 reclaimable leases (claim issuance, attach, rotate, revoke)', () => {
    /** Load the adapter + mocked Channel class together. The dynamic imports
     * keep the vi.mock factories lazy (they only run after the module-level
     * mock fns above are initialized). */
    async function loadApi() {
      const [{ createTauriTerminalApi }, { Channel }] = await Promise.all([
        import('../tauri-terminal-api'),
        import('@tauri-apps/api/core')
      ])
      return { api: createTauriTerminalApi(), Channel }
    }

    type ChannelLike = { onmessage: ((message: ArrayBuffer) => void) | null }

    const SPAWNED = {
      id: 'terminal-1752-1',
      shell: 'pwsh',
      cwd: 'C:/dev/project',
      pid: 4242,
      cols: 120,
      rows: 32,
      claim: 'issued-claim-64-hex'
    }

    it('spawn result surfaces the issued claim alongside terminal info', async () => {
      const { api, Channel } = await loadApi()
      mockInvoke.mockResolvedValue({ success: true, data: SPAWNED })

      const result = await api.spawn({
        conversationId: '018f7a1c-1b4d-7c8a-9f01-0123456789ab',
        projectId: 'p1',
        cols: 120,
        rows: 32
      })

      expect(mockInvoke).toHaveBeenCalledTimes(1)
      const [command, args] = mockInvoke.mock.calls[0]
      expect(command).toBe('terminal_spawn')
      expect(args.options).toEqual({
        conversationId: '018f7a1c-1b4d-7c8a-9f01-0123456789ab',
        projectId: 'p1',
        cols: 120,
        rows: 32
      })
      // Output still streams through a raw binary channel (shape unchanged).
      expect(args.onData).toBeInstanceOf(Channel)

      // CAP-3: spawn is the only issuance path — the claim rides alongside
      // the terminal info fields in the same flattened camelCase shape.
      expect(result).toEqual({ success: true, data: SPAWNED })
      if (result.success) {
        expect(result.data.claim).toBe('issued-claim-64-hex')
        expect(result.data.id).toBe('terminal-1752-1')
      }
    })

    it('resumes with the exact scoped request, replays first, then attaches live from latestSeq', async () => {
      const { api, Channel } = await loadApi()
      const request = {
        conversationId: '018f7a1c-1b4d-7c8a-9f01-0123456789ab',
        terminalId: 'terminal-1752-1',
        lastSeq: 12
      }
      const grant = {
        terminal: {
          id: 'terminal-1752-1',
          shell: 'pwsh',
          cwd: 'C:/dev/project',
          pid: 4242,
          cols: 120,
          rows: 32,
          latestSeq: 87,
          gap: false
        },
        claim: 'resume-claim-rotated'
      }
      mockInvoke
        .mockImplementationOnce(async (_command: string, args: Record<string, unknown>) => {
          const channel = args.onData as unknown as ChannelLike
          channel.onmessage?.(new Uint8Array([114, 101, 112, 108, 97, 121]).buffer)
          return { success: true, data: grant }
        })
        .mockResolvedValueOnce({ success: true, data: grant.terminal })

      const received: Array<{ terminalId: string; bytes: Uint8Array }> = []
      const off = api.onData((terminalId, bytes) => received.push({ terminalId, bytes }))

      const result = await api.resume(request)

      expect(result).toEqual({ success: true, data: grant })
      expect(mockInvoke).toHaveBeenCalledTimes(2)
      const [resumeCommand, resumeArgs] = mockInvoke.mock.calls[0]
      expect(resumeCommand).toBe('terminal_resume')
      expect(resumeArgs.request).toEqual(request)
      expect(resumeArgs.onData).toBeInstanceOf(Channel)
      expect(resumeArgs).not.toHaveProperty('program')
      expect(resumeArgs).not.toHaveProperty('env')

      const [attachCommand, attachArgs] = mockInvoke.mock.calls[1]
      expect(attachCommand).toBe('terminal_attach')
      expect(attachArgs).toMatchObject({
        terminalId: 'terminal-1752-1',
        claim: 'resume-claim-rotated',
        lastSeq: 87
      })
      expect(attachArgs.onData).toBeInstanceOf(Channel)
      ;(attachArgs.onData as unknown as ChannelLike).onmessage?.(
        new Uint8Array([108, 105, 118, 101]).buffer
      )
      expect(received.map(({ terminalId }) => terminalId)).toEqual([
        'terminal-1752-1',
        'terminal-1752-1'
      ])
      expect(received.map(({ bytes }) => new TextDecoder().decode(bytes))).toEqual([
        'replay',
        'live'
      ])

      off()
    })

    it('returns generic UNAUTHORIZED for denied resume and never attempts attach', async () => {
      const { api } = await loadApi()
      mockInvoke.mockResolvedValue({ success: false, error: 'Unauthorized', code: 'UNAUTHORIZED' })
      const received: Uint8Array[] = []
      const off = api.onData((_terminalId, bytes) => received.push(bytes))

      const result = await api.resume({
        conversationId: '018f7a1c-1b4d-7c8a-9f01-0123456789ab',
        terminalId: 'unknown-or-wrong-scope',
        lastSeq: 0
      })

      expect(result).toEqual({ success: false, error: 'Unauthorized', code: 'UNAUTHORIZED' })
      expect(mockInvoke).toHaveBeenCalledTimes(1)
      expect(mockInvoke.mock.calls[0][0]).toBe('terminal_resume')
      const replayChannel = mockInvoke.mock.calls[0][1].onData as unknown as ChannelLike
      replayChannel.onmessage?.(new Uint8Array([1, 2, 3]).buffer)
      expect(received).toHaveLength(0)

      off()
    })

    it('fails closed on a mismatched resume grant and never attaches it', async () => {
      const { api } = await loadApi()
      mockInvoke.mockResolvedValue({
        success: true,
        data: {
          terminal: {
            id: 'different-terminal',
            shell: 'bash',
            cwd: '/tmp',
            pid: 1,
            cols: 80,
            rows: 24,
            latestSeq: 4,
            gap: false
          },
          claim: 'wrong-terminal-grant'
        }
      })

      const result = await api.resume({
        conversationId: '018f7a1c-1b4d-7c8a-9f01-0123456789ab',
        terminalId: 'expected-terminal',
        lastSeq: 0
      })

      expect(result).toEqual({
        success: false,
        error: 'Terminal resume failed',
        code: 'NETWORK_ERROR'
      })
      expect(mockInvoke).toHaveBeenCalledTimes(1)
      expect(mockInvoke.mock.calls[0][0]).toBe('terminal_resume')
    })

    it('attach passes terminalId + claim + lastSeq + Channel to terminal_attach and streams bytes by terminal id', async () => {
      const { api, Channel } = await loadApi()
      mockInvoke.mockResolvedValue({
        success: true,
        data: {
          id: 'terminal-1752-1',
          shell: 'pwsh',
          cwd: 'C:/dev/project',
          pid: 4242,
          cols: 120,
          rows: 32,
          latestSeq: 87,
          gap: false
        }
      })

      const received: Array<{ terminalId: string; bytes: Uint8Array }> = []
      const off = api.onData((terminalId, bytes) => received.push({ terminalId, bytes }))

      const result = await api.attach('terminal-1752-1', 'lease-claim-64-hex', 12)

      expect(mockInvoke).toHaveBeenCalledTimes(1)
      const [command, args] = mockInvoke.mock.calls[0]
      expect(command).toBe('terminal_attach')
      // The credential gate: id + claim + cursor — never id-only.
      expect(args).toMatchObject({
        terminalId: 'terminal-1752-1',
        claim: 'lease-claim-64-hex',
        lastSeq: 12
      })
      expect(args.onData).toBeInstanceOf(Channel)

      // Attach result: replay cursor + gap flag in camelCase — and it NEVER
      // carries a claim (attach consumes the credential, never issues one).
      expect(result.success).toBe(true)
      if (result.success) {
        expect(result.data).toEqual({
          id: 'terminal-1752-1',
          shell: 'pwsh',
          cwd: 'C:/dev/project',
          pid: 4242,
          cols: 120,
          rows: 32,
          latestSeq: 87,
          gap: false
        })
        expect('claim' in result.data).toBe(false)
      }

      // Streamed bytes reach data callbacks keyed by the attached terminal id.
      const channel = args.onData as unknown as ChannelLike
      channel.onmessage?.(new Uint8Array([104, 105]).buffer)
      expect(received).toHaveLength(1)
      expect(received[0].terminalId).toBe('terminal-1752-1')
      expect(Array.from(received[0].bytes)).toEqual([104, 105])

      off()
    })

    it('routes binary channel output only to matching scoped subscribers', async () => {
      const { api } = await loadApi()
      mockInvoke.mockResolvedValue({
        success: true,
        data: {
          id: 'terminal-1752-1',
          shell: 'pwsh',
          cwd: 'C:/dev/project',
          pid: 4242,
          cols: 120,
          rows: 32,
          latestSeq: 1,
          gap: false
        }
      })
      const global = vi.fn()
      const matching = vi.fn()
      const unrelated = vi.fn()
      const offGlobal = api.onData(global)
      const offMatching = api.onDataForTerminal?.('terminal-1752-1', matching)
      const offUnrelated = api.onDataForTerminal?.('terminal-other', unrelated)

      await api.attach('terminal-1752-1', 'lease-claim-64-hex', 0)
      const channel = mockInvoke.mock.calls[0][1].onData as unknown as ChannelLike
      channel.onmessage?.(new Uint8Array([104, 105]).buffer)

      expect(global).toHaveBeenCalledTimes(1)
      expect(global).toHaveBeenCalledWith('terminal-1752-1', expect.any(Uint8Array))
      expect(matching).toHaveBeenCalledTimes(1)
      expect(matching).toHaveBeenCalledWith(expect.any(Uint8Array))
      expect(unrelated).not.toHaveBeenCalled()

      offGlobal()
      offMatching?.()
      offUnrelated?.()
    })

    it('never presents an id-only attach: empty claim fails without an invoke', async () => {
      const { api } = await loadApi()

      const result = await api.attach('terminal-1752-1', '', 0)

      expect(result).toEqual({ success: false, error: 'Unauthorized', code: 'UNAUTHORIZED' })
      expect(mockInvoke).not.toHaveBeenCalled()
    })

    it('attach failure surfaces the generic UNAUTHORIZED IpcResult error and releases the channel', async () => {
      const { api } = await loadApi()
      mockInvoke.mockResolvedValue({ success: false, error: 'Unauthorized', code: 'UNAUTHORIZED' })

      const received: Array<{ terminalId: string; bytes: Uint8Array }> = []
      const off = api.onData((terminalId, bytes) => received.push({ terminalId, bytes }))

      const result = await api.attach('terminal-1752-1', 'stolen-or-rotated-claim', 0)

      // The host's single generic rejection surfaces verbatim — no detail
      // distinguishing unknown terminal vs wrong/revoked claim (no leak).
      expect(result).toEqual({ success: false, error: 'Unauthorized', code: 'UNAUTHORIZED' })

      // The stream channel is released: a late byte can never reach callbacks.
      const channel = mockInvoke.mock.calls[0][1].onData as unknown as ChannelLike
      expect(typeof channel.onmessage).toBe('function') // swapped for a no-op
      channel.onmessage?.(new Uint8Array([1]).buffer)
      expect(received).toHaveLength(0)

      off()
    })

    it('maps closeView and explicit terminate to distinct commands; kill remains an alias', async () => {
      const { api } = await loadApi()
      mockInvoke.mockResolvedValue({ success: true, data: undefined })

      await api.closeView('terminal-1752-1')
      await api.terminate('terminal-1752-1')
      await api.kill('terminal-1752-1')

      expect(mockInvoke).toHaveBeenNthCalledWith(1, 'terminal_close_view', {
        terminalId: 'terminal-1752-1'
      })
      expect(mockInvoke).toHaveBeenNthCalledWith(2, 'terminal_terminate', {
        terminalId: 'terminal-1752-1'
      })
      expect(mockInvoke).toHaveBeenNthCalledWith(3, 'terminal_kill', {
        terminalId: 'terminal-1752-1'
      })
    })

    it('preserves an ordinary cleanup failure against the existing terminal identity', async () => {
      const { api } = await loadApi()
      const cleanupFailure = {
        success: false as const,
        code: 'TERMINATE_FAILED',
        error: JSON.stringify({
          terminalId: 'terminal-1752-1',
          primaryCode: 'TERMINATE_FAILED',
          cleanupStage: 'reader_join'
        })
      }
      mockInvoke
        .mockResolvedValueOnce({ success: true, data: SPAWNED })
        .mockResolvedValueOnce(cleanupFailure)

      const spawned = await api.spawn()
      const terminated = await api.terminate('terminal-1752-1')

      expect(spawned.success).toBe(true)
      expect(terminated).toBe(cleanupFailure)
      expect(JSON.parse(terminated.success ? '{}' : terminated.error)).toEqual({
        terminalId: 'terminal-1752-1',
        primaryCode: 'TERMINATE_FAILED',
        cleanupStage: 'reader_join'
      })
      expect(mockInvoke.mock.calls.map(([command]) => command)).toEqual([
        'terminal_spawn',
        'terminal_terminate'
      ])
    })

    it('retries cleanup through invoke with the exact retained id and never spawns a replacement', async () => {
      const { api } = await loadApi()
      const cleanupFailure = {
        success: false as const,
        code: 'TERMINATE_FAILED',
        error: JSON.stringify({
          terminalId: 'terminal-retained-retry',
          primaryCode: 'TERMINATE_FAILED',
          cleanupStage: 'flusher_join'
        })
      }
      mockInvoke
        .mockResolvedValueOnce(cleanupFailure)
        .mockResolvedValueOnce({ success: true, data: undefined })

      const failed = await api.terminate('terminal-retained-retry')
      const succeeded = await api.terminate('terminal-retained-retry')

      expect(failed).toBe(cleanupFailure)
      expect(succeeded).toEqual({ success: true, data: undefined })
      expect(mockInvoke.mock.calls).toEqual([
        ['terminal_terminate', { terminalId: 'terminal-retained-retry' }],
        ['terminal_terminate', { terminalId: 'terminal-retained-retry' }]
      ])
      expect(mockInvoke.mock.calls.some(([command]) => command === 'terminal_spawn')).toBe(false)
      expect(mockInvoke.mock.calls.some(([command]) => command === 'terminal_attach')).toBe(false)
      expect(JSON.parse(failed.success ? '{}' : failed.error)).toEqual({
        terminalId: 'terminal-retained-retry',
        primaryCode: 'TERMINATE_FAILED',
        cleanupStage: 'flusher_join'
      })
    })

    it('preserves compound rollback detail without spawning a replacement terminal', async () => {
      const { api } = await loadApi()
      const compoundFailure = {
        success: false as const,
        code: 'TERMINAL_RESOURCE_ROLLBACK_FAILED',
        error: JSON.stringify({
          terminalId: 'terminal-recoverable-1',
          primaryCode: 'CONVERSATION_DURABILITY_FAILED',
          cleanupStage: 'kill'
        })
      }
      mockInvoke.mockResolvedValueOnce(compoundFailure)

      const result = await api.spawn({
        conversationId: '018f7a1c-1b4d-7c8a-9f01-0123456789ab'
      })

      expect(result).toBe(compoundFailure)
      expect(mockInvoke).toHaveBeenCalledTimes(1)
      expect(mockInvoke.mock.calls[0][0]).toBe('terminal_spawn')
      expect(JSON.parse(result.success ? '{}' : result.error)).toEqual({
        terminalId: 'terminal-recoverable-1',
        primaryCode: 'CONVERSATION_DURABILITY_FAILED',
        cleanupStage: 'kill'
      })
    })

    it('rotateClaim invokes terminal_rotate_claim with (terminalId, claim) and returns the fresh credential', async () => {
      const { api } = await loadApi()
      mockInvoke.mockResolvedValue({ success: true, data: { claim: 'rotated-claim-64-hex' } })

      const result = await api.rotateClaim('terminal-1752-1', 'lease-claim-64-hex')

      expect(mockInvoke).toHaveBeenCalledTimes(1)
      expect(mockInvoke).toHaveBeenCalledWith('terminal_rotate_claim', {
        terminalId: 'terminal-1752-1',
        claim: 'lease-claim-64-hex'
      })
      // Possession-based rotation: the response carries the fresh credential.
      expect(result).toEqual({ success: true, data: { claim: 'rotated-claim-64-hex' } })
    })

    it('revokeClaim invokes terminal_revoke_claim with (terminalId, claim)', async () => {
      const { api } = await loadApi()
      mockInvoke.mockResolvedValue({ success: true, data: undefined })

      const result = await api.revokeClaim('terminal-1752-1', 'lease-claim-64-hex')

      expect(mockInvoke).toHaveBeenCalledTimes(1)
      expect(mockInvoke).toHaveBeenCalledWith('terminal_revoke_claim', {
        terminalId: 'terminal-1752-1',
        claim: 'lease-claim-64-hex'
      })
      expect(result).toEqual({ success: true, data: undefined })
    })
  })
})
