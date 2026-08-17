import type { ConversationId } from './conversation.types'
import type {
  GitStatus,
  IpcResult,
  RotatedClaim,
  SpawnedTerminal,
  TerminalAttachResult,
  TerminalResumeGrant,
  TerminalResumeRequest
} from './ipc.types'

export type { TerminalResumeGrant, TerminalResumeRequest } from './ipc.types'

export type WebTerminalRequestType =
  | 'spawn'
  | 'resume'
  | 'write'
  | 'resize'
  | 'terminate'
  | 'kill'
  | 'attach'
  | 'detach'
  | 'close_view'
  | 'rotate_claim'
  | 'revoke_claim'
  | 'get_cwd'
  | 'get_git_branch'
  | 'get_git_status'
  | 'get_exit_code'
  | 'add_renderer_ref'
  | 'remove_renderer_ref'
  | 'set_protected'
  | 'update_orphan_detection'

export type TerminalCwdSource = 'workspace' | 'executionTarget'

/** Exact sanitized PTY cleanup stages emitted by both native transports. */
export const TERMINAL_CLEANUP_STAGES = ['kill', 'wait', 'flusher_join', 'reader_join'] as const

export type TerminalCleanupStage = (typeof TERMINAL_CLEANUP_STAGES)[number]

export const TERMINAL_RESOURCE_FAILURE_CODES = [
  'TERMINATE_FAILED',
  'TERMINAL_RESOURCE_ROLLBACK_FAILED'
] as const

export type TerminalResourceFailureCode = (typeof TERMINAL_RESOURCE_FAILURE_CODES)[number]

/**
 * Secret-safe recoverable resource detail. The backend deliberately omits the
 * claim, process, command, argv, cwd, environment, output, and Conversation id.
 */
export interface TerminalResourceFailureV1 {
  terminalId: string
  primaryCode: string
  cleanupStage: TerminalCleanupStage
}

/**
 * Decode only the exact cleanup/compound error contract without rewriting the
 * original IpcResult. Callers can retain the recoverable terminal identity
 * while forwarding the stable transport envelope byte-for-byte.
 */
export function readTerminalResourceFailure(
  result: IpcResult<unknown>
): TerminalResourceFailureV1 | null {
  if (
    result.success ||
    !TERMINAL_RESOURCE_FAILURE_CODES.includes(result.code as TerminalResourceFailureCode)
  ) {
    return null
  }

  let value: unknown
  try {
    value = JSON.parse(result.error)
  } catch {
    return null
  }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null

  const record = value as Record<string, unknown>
  const keys = Object.keys(record).sort()
  if (keys.join(',') !== 'cleanupStage,primaryCode,terminalId') return null
  if (
    typeof record.terminalId !== 'string' ||
    record.terminalId.length === 0 ||
    typeof record.primaryCode !== 'string' ||
    record.primaryCode.length === 0 ||
    typeof record.cleanupStage !== 'string' ||
    !TERMINAL_CLEANUP_STAGES.includes(record.cleanupStage as TerminalCleanupStage)
  ) {
    return null
  }

  return {
    terminalId: record.terminalId,
    primaryCode: record.primaryCode,
    cleanupStage: record.cleanupStage as TerminalCleanupStage
  }
}

/**
 * Remote spawn authority is intentionally narrow. The host resolves cwd from
 * the Conversation and derives shell/program/argv/environment itself.
 */
export interface TerminalSpawnIntentV1 {
  conversationId: ConversationId
  projectId?: string
  cwdSource: TerminalCwdSource
  cols: number
  rows: number
}

export type WebTerminalRequest =
  | { id: string; type: 'spawn'; payload: TerminalSpawnIntentV1 }
  | { id: string; type: 'resume'; payload: TerminalResumeRequest }
  | {
      id: string
      type: Exclude<WebTerminalRequestType, 'spawn' | 'resume'>
      payload: Record<string, unknown>
    }

export type WebTerminalReply<T = unknown> =
  | { id: string; success: true; data: T }
  | { id: string; success: false; error: string; code: string }

/** A single sequenced output chunk (live data frame). */
export interface WebTerminalDataFrame {
  type: 'data'
  terminalId: string
  seq: number
  data: number[]
}

/** Sequenced replay frame: unseen chunks sent on attach/reconnect. */
export interface WebTerminalReplayFrame {
  type: 'replay'
  terminalId: string
  chunks: Array<{ seq: number; data: number[] }>
  gap: boolean
  latestSeq: number
  snapshot: WebTerminalStateSnapshot
}

/** Gap marker frame: broadcast receiver lagged, some output was lost. */
export interface WebTerminalGapFrame {
  type: 'gap'
  terminalId: string
  lastSeq: number
}

/** Latest lifecycle/metadata state (sent with replay). */
export interface WebTerminalStateSnapshot {
  cwd: string | null
  gitBranch: string | null
  gitStatus: GitStatus | null
  exitCode: number | null
  exited: boolean
}

export type WebTerminalEventPayload =
  | { type: 'exit'; terminal_id: string; exit_code: number | null; signal: number | null }
  | { type: 'cwd_changed'; terminal_id: string; cwd: string }
  | { type: 'git_branch_changed'; terminal_id: string; branch: string | null }
  | { type: 'git_status_changed'; terminal_id: string; status: GitStatus | null }
  | { type: 'exit_code_changed'; terminal_id: string; exit_code: number }

export interface WebTerminalEventFrame {
  type: 'event'
  payload: WebTerminalEventPayload
}

export type WebTerminalFrame<T = unknown> =
  | WebTerminalReply<T>
  | WebTerminalDataFrame
  | WebTerminalReplayFrame
  | WebTerminalGapFrame
  | WebTerminalEventFrame

/**
 * CAP-3: the spawn reply carries the issued claim credential (flattened
 * camelCase, same shape as the desktop `terminal_spawn` IpcResult data).
 */
export type WebTerminalSpawnReply = WebTerminalReply<SpawnedTerminal>

/** CAP-3: attach reply — shared TerminalAttachResult shape (never a claim). */
export type WebTerminalAttachReply = WebTerminalReply<TerminalAttachResult>

/** Authenticated cold-resume reply — identical to desktop TerminalResumeGrant. */
export type WebTerminalResumeReply = WebTerminalReply<TerminalResumeGrant>

/** CAP-3: rotate reply — the fresh credential. */
export type WebTerminalRotateClaimReply = WebTerminalReply<RotatedClaim>
