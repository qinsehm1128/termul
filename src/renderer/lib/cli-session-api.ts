/**
 * CLI session discovery facade.
 *
 * Desktop uses Tauri IPC (`list_cli_sessions_cmd`); web/remote uses
 * `POST /cli-sessions`. Both return the same parsed list result.
 */
import type {
  CliSessionApi,
  CliSessionListArgs,
  CliSessionListResult
} from '@shared/types/cli-session.types'

import { createTauriCliSessionApi } from './tauri-cli-session-api'
import { isTauriContext } from './tauri-runtime'
import { webCliSessionApi } from './web-cli-session-api'

const tauriCliSessionApi = createTauriCliSessionApi()

export const cliSessionApi: CliSessionApi = {
  listSessions(args?: CliSessionListArgs): Promise<CliSessionListResult> {
    if (!isTauriContext()) return webCliSessionApi.listSessions(args)
    return tauriCliSessionApi.listSessions(args)
  }
}

export { createTauriCliSessionApi } from './tauri-cli-session-api'
export { webCliSessionApi } from './web-cli-session-api'
