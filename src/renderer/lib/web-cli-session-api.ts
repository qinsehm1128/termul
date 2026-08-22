/**
 * Web/remote HTTP adapter for CLI session discovery.
 *
 * Hits `POST /cli-sessions` via `webServerCliSessions`.
 */
import type { CliSessionApi } from '@shared/types/cli-session.types'

import { webServerCliSessions } from './web-server-api'

export const webCliSessionApi: CliSessionApi = {
  listSessions(args) {
    return webServerCliSessions.list(args)
  }
}
