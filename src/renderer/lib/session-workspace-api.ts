import type { SessionWorkspaceApi } from '@shared/types/session-workspace.types'
import { isTauriContext } from './tauri-runtime'
import { createTauriSessionWorkspaceApi } from './tauri-session-workspace-api'
import { webSessionWorkspaceApi } from './web-session-workspace-api'

export const sessionWorkspaceApi: SessionWorkspaceApi = isTauriContext()
  ? createTauriSessionWorkspaceApi()
  : webSessionWorkspaceApi

export { createTauriSessionWorkspaceApi } from './tauri-session-workspace-api'
export { webSessionWorkspaceApi } from './web-session-workspace-api'
