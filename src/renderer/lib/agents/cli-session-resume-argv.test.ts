import type { DiscoveredCliSession } from '@shared/types/cli-session.types'
import { describe, expect, it } from 'vitest'
import { getBuiltInAgent } from '@/lib/agents/agent-registry'
import {
  buildCliResumeArgv,
  normalizeResumeFilePath,
  resumeHandleForSession
} from '@/lib/agents/cli-session-resume-argv'

function session(over: Partial<DiscoveredCliSession> = {}): DiscoveredCliSession {
  return {
    schemaVersion: 1,
    id: 'claude-code:abc:/tmp/a.jsonl',
    agentId: 'claude-code',
    sessionId: 'abc',
    cwd: '/repo',
    title: 'Hello',
    createdAt: null,
    updatedAt: null,
    messageCount: 1,
    filePath: '/tmp/a.jsonl',
    resumable: true,
    ...over
  }
}

describe('buildCliResumeArgv', () => {
  it('places extras before the resume flag and session id', () => {
    const def = getBuiltInAgent('claude-code')!
    const built = buildCliResumeArgv(def, session(), '--dangerously-skip-permissions', '--verbose')
    expect(built).toEqual({
      program: 'claude',
      args: ['--dangerously-skip-permissions', '--verbose', '--resume', 'abc']
    })
  })

  it('uses a subcommand for Codex', () => {
    const def = getBuiltInAgent('codex')!
    const built = buildCliResumeArgv(def, session({ agentId: 'codex', sessionId: 's1' }), '', '')
    expect(built).toEqual({ program: 'codex', args: ['resume', 's1'] })
  })

  it('uses --session and the transcript path for pi', () => {
    const def = getBuiltInAgent('pi')!
    const built = buildCliResumeArgv(
      def,
      session({
        agentId: 'pi',
        sessionId: 'file',
        filePath: '/home/me/.pi/agent/sessions/a.jsonl',
        resumeFilePath: '/home/me/.pi/agent/sessions/a.jsonl'
      }),
      '',
      ''
    )
    expect(built).toEqual({
      program: 'pi',
      args: ['--session', '/home/me/.pi/agent/sessions/a.jsonl']
    })
  })

  it('rejects a leading-dash session id', () => {
    const def = getBuiltInAgent('claude-code')!
    expect(buildCliResumeArgv(def, session({ sessionId: '-evil' }), '', '')).toEqual({
      error: 'Session is missing a safe resume handle'
    })
  })
})

describe('resume path guards', () => {
  it('rejects relative and parent paths', () => {
    expect(normalizeResumeFilePath('../x')).toBeNull()
    expect(normalizeResumeFilePath('/ok/../x')).toBeNull()
    expect(normalizeResumeFilePath('/ok/a.jsonl')).toBe('/ok/a.jsonl')
  })

  it('uses resumeFilePath for pi', () => {
    expect(
      resumeHandleForSession(
        session({
          agentId: 'pi',
          resumeFilePath: '/abs/session.jsonl',
          filePath: '/abs/session.jsonl'
        })
      )
    ).toBe('/abs/session.jsonl')
  })
})
