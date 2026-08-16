import { useEffect } from 'react'
import { useUpdateToast } from '@/components/UpdateAvailableToast'
import { useAcpAgents } from '@/hooks/use-acp-agents'
import { useAcpHistory } from '@/hooks/use-acp-history'
import { useAcpListeners } from '@/hooks/use-acp-listeners'
import { useAcpMcp } from '@/hooks/use-acp-mcp'
import { useAcpSessionResume } from '@/hooks/use-acp-session-resume'
import { useAppSettingsLoader } from '@/hooks/use-app-settings'
import { useAppliedColorThemeSync } from '@/hooks/use-color-theme'
import { useContextBarSettings } from '@/hooks/use-context-bar-settings'
import { useConversationHostBootstrap } from '@/hooks/use-conversation-host-bootstrap'
import { useConversationLifecycle } from '@/hooks/use-conversation-lifecycle'
import { useCrashRecovery } from '@/hooks/use-crash-recovery'
import { useCwd } from '@/hooks/use-cwd'
import { useExitCode } from '@/hooks/use-exit-code'
import { useGitBranch } from '@/hooks/use-git-branch'
import { useGitStatus } from '@/hooks/use-git-status'
import { useKeyboardShortcutsLoader } from '@/hooks/use-keyboard-shortcuts'
import { useAppliedLanguageSync } from '@/hooks/use-language'
import { useMenuUpdaterListener } from '@/hooks/use-menu-updater-listener'
import { usePreventFileDropNavigation } from '@/hooks/use-prevent-file-drop-navigation'
import { usePreventNativeContextMenu } from '@/hooks/use-prevent-native-context-menu'
import { useProjectsAutoSave, useProjectsLoader } from '@/hooks/use-projects-persistence'
import { useRemoteProjects } from '@/hooks/use-remote-projects'
import { useSessionWorkspaceBootstrap } from '@/hooks/use-session-workspace-sync'
import { useTerminalDetachedOutput } from '@/hooks/use-terminal-detached-output'
import { useTerminalExitNotification } from '@/hooks/use-terminal-exit-notification'
import { useTerminalResourceLifecycle } from '@/hooks/use-terminal-resource-lifecycle'
import { useTerminalRestore } from '@/hooks/use-terminal-restore'
import { useAppliedUiZoomSync } from '@/hooks/use-ui-zoom'
import { useUpdateCheck } from '@/hooks/use-updater'
import { useVisibilityState } from '@/hooks/use-visibility-state'
import { useTerminalAutoSave } from '@/hooks/useTerminalAutoSave'
import { initNotificationPermissions } from '@/lib/tauri-notification-api'

/** Portable application effects, mounted in this fixed order by every renderer root. */
export function PortableAppEffects(): null {
  useTerminalAutoSave()
  useSessionWorkspaceBootstrap()
  useConversationHostBootstrap()
  useConversationLifecycle()
  useTerminalResourceLifecycle()
  useTerminalRestore()
  useCrashRecovery()
  useTerminalDetachedOutput()
  useCwd()
  useGitBranch()
  useGitStatus()
  useExitCode()
  useContextBarSettings()
  useAppSettingsLoader()
  useAppliedLanguageSync()
  useAppliedColorThemeSync()
  useAppliedUiZoomSync()
  useKeyboardShortcutsLoader()
  useProjectsLoader()
  useProjectsAutoSave()
  useMenuUpdaterListener()
  useUpdateCheck()
  useUpdateToast()
  useVisibilityState()
  useTerminalExitNotification()
  useRemoteProjects()
  useAcpListeners()
  useAcpAgents()
  useAcpHistory()
  useAcpSessionResume()
  useAcpMcp()
  usePreventFileDropNavigation()
  // Bubble-phase suppression preserves the Radix global context-menu trigger
  // while covering portaled overlays on both browser and native webview roots.
  usePreventNativeContextMenu()

  useEffect(() => {
    void initNotificationPermissions()
  }, [])

  return null
}
