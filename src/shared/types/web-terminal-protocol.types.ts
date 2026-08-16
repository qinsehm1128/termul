import type { ConversationId } from './conversation.types'
import type { GitStatus, RotatedClaim, SpawnedTerminal, TerminalAttachResult } from './ipc.types'

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

/** Cold-renderer request for a host-authorized, one-time claim rotation. */
export interface TerminalResumeRequest {
  conversationId: ConversationId
  terminalId: string
  lastSeq: number
}

/**
 * Authenticated resume handoff. `claim` is response-only and in-memory-only;
 * it must never be added to SessionWorkspace or renderer persistence.
 */
export interface TerminalResumeGrant {
  terminal: TerminalAttachResult
  claim: string
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
