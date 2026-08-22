/**
 * Desktop Tauri adapter for CLI session discovery.
 *
 * The host command already returns `CliSessionListResult` (or a thrown string).
 * Invoke failures map to a thrown Error so the facade can log and surface them.
 */
import type {
  CliSessionApi,
  CliSessionListArgs,
  CliSessionListResult
} from '@shared/types/cli-session.types'
import { parseCliSessionListResult } from '@shared/types/cli-session.types'
import { invoke } from '@tauri-apps/api/core'

export function createTauriCliSessionApi(): CliSessionApi {
  return {
    async listSessions(args?: CliSessionListArgs): Promise<CliSessionListResult> {
      const raw = await invoke<unknown>('list_cli_sessions_cmd', { args: args ?? null })
      const parsed = parseCliSessionListResult(raw)
      if (!parsed) {
        throw new Error('CLI session scan returned an invalid payload')
      }
      return parsed
    }
  }
}
