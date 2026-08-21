/**
 * Transport-neutral terminal facade.
 *
 * View attachment/detachment is deliberately independent from PTY resource
 * termination. Renderer refs route through the active runtime; neither helper
 * destroys a terminal.
 */

import type {
  IpcResult,
  TerminalApi,
  TerminalResumeGrant,
  TerminalResumeRequest,
  TerminalScopedDataCallback
} from '@shared/types/ipc.types'
import { isTauriContext } from './tauri-runtime'
import {
  addRendererRef as addTauriRendererRef,
  createTauriTerminalApi,
  removeRendererRef as removeTauriRendererRef,
  setTerminalProtected as setTauriTerminalProtected
} from './tauri-terminal-api'
import { createWebTerminalApi, webTerminalInternals } from './web-terminal-api'

export const terminalApi: TerminalApi = isTauriContext()
  ? createTauriTerminalApi()
  : createWebTerminalApi()

/** Transport-neutral cold-resume entry point; never spawns or terminates a PTY. */
export function resumeTerminal(
  request: TerminalResumeRequest
): Promise<IpcResult<TerminalResumeGrant>> {
  return terminalApi.resume(request)
}

/** Subscribe directly to one PTY, falling back to transport-wide filtering. */
export function subscribeTerminalData(
  terminalId: string,
  callback: TerminalScopedDataCallback
): () => void {
  if (terminalApi.onDataForTerminal) {
    return terminalApi.onDataForTerminal(terminalId, callback)
  }
  return terminalApi.onData((candidateId, data) => {
    if (candidateId === terminalId) callback(data)
  })
}

export function addRendererRef(terminalId: string, rendererId: string): Promise<IpcResult<void>> {
  return isTauriContext()
    ? addTauriRendererRef(terminalId, rendererId)
    : webTerminalInternals.addRendererRef(terminalId, rendererId)
}

export function removeRendererRef(
  terminalId: string,
  rendererId: string
): Promise<IpcResult<void>> {
  return isTauriContext()
    ? removeTauriRendererRef(terminalId, rendererId)
    : webTerminalInternals.removeRendererRef(terminalId, rendererId)
}

export function setTerminalProtected(
  terminalId: string,
  protectedState: boolean
): Promise<IpcResult<void>> {
  return isTauriContext()
    ? setTauriTerminalProtected(terminalId, protectedState)
    : webTerminalInternals.setProtected(terminalId, protectedState)
}
