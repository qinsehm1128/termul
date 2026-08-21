import {
  FolderKanban,
  GitBranch,
  History,
  MessageSquarePlus,
  Network,
  Palette,
  SlidersHorizontal
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { TermulMark } from '@/components/TermulMark'
import { TitleBarShortcutsPopover } from '@/components/TitleBarShortcutsPopover'
import { useUpdatePanelVisibility } from '@/hooks/use-app-settings'
import { isMac } from '@/lib/platform'
import { isTauriContext } from '@/lib/tauri-runtime'
import { useSSHPanelVisible } from '@/stores/ssh-panel-store'

const railButtonClass =
  'mx-1 flex h-9 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground focus:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-35'

interface ActivityRailProps {
  isShortcutsOpen?: boolean
  onShortcutsOpenChange?: (open: boolean) => void
  /** Opens the command palette (project switcher / launcher). */
  onOpenCommandPalette?: () => void
  /** Opens a git changes tab in the active pane. */
  onOpenGitChanges?: () => void
  /** Whether a git changes tab can currently be opened (active project has a path). */
  canOpenGitChanges?: boolean
  /** Opens the New Agent Chat dialog. */
  onOpenAgentChat?: () => void
  /** Whether a new agent chat can currently be started (active project has a path). */
  canOpenAgentChat?: boolean
  /** Opens a git history (commit graph) tab in the active pane. */
  onOpenGitHistory?: () => void
  /** Whether a git history tab can currently be opened (active project has a path). */
  canOpenGitHistory?: boolean
  /** Whether the color theme picker overlay is open. */
  isThemePickerOpen?: boolean
  /** Toggle the color theme picker (opens beside the rail). */
  onToggleThemePicker?: () => void
}

/**
 * Vertical activity rail (VSCode-style) that hosts the app's global actions.
 *
 * Layout:
 * - macOS: WorkspaceLayout renders a full-width titlebar zone above this rail;
 *   the brand row stays draggable for top-left window moves.
 * - Brand mark at the top, followed by a separator.
 * - Top group: projects (command palette), git changes, SSH panel toggle.
 * - Bottom group (pinned via `mt-auto`): keyboard shortcuts, preferences,
 *   color themes. Sidebar/file-explorer visibility toggles moved to the
 *   titlebar strip (TitleBar / MacOsTitlebarStrip) beside the OS window
 *   controls.
 *
 * The SSH panel toggle preserves the persistence-aware updater, error-toast,
 * and accessible-label contracts that previously lived in the top title bar.
 * Sidebar/file-explorer visibility toggles now live in the titlebar strip.
 */
export function ActivityRail({
  isShortcutsOpen,
  onShortcutsOpenChange,
  onOpenCommandPalette,
  onOpenGitChanges,
  canOpenGitChanges = false,
  onOpenAgentChat,
  canOpenAgentChat = false,
  onOpenGitHistory,
  canOpenGitHistory = false,
  isThemePickerOpen = false,
  onToggleThemePicker
}: ActivityRailProps = {}): React.JSX.Element {
  const { t } = useTranslation('shell')
  const isSSHPanelVisible = useSSHPanelVisible()
  const updatePanelVisibility = useUpdatePanelVisibility()
  const navigate = useNavigate()
  const location = useLocation()

  const handleToggleSSHPanel = async (e: React.MouseEvent<HTMLButtonElement>): Promise<void> => {
    e.stopPropagation()
    try {
      await updatePanelVisibility('sshPanelVisible', !isSSHPanelVisible)
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t('activityRail.failedSsh'))
    }
  }

  return (
    <nav
      className="w-11 flex flex-col items-center border-r border-sidebar-border/70 bg-sidebar select-none shrink-0"
      aria-label={t('activityRail.globalActions')}
    >
      {/* Brand mark */}
      <div
        className="flex h-9 w-11 shrink-0 items-center justify-center text-foreground"
        data-tauri-drag-region={isMac ? true : undefined}
      >
        <TermulMark size={19} className="pointer-events-none" />
      </div>

      <div className="my-1 h-px w-5 bg-border/60" aria-hidden="true" />

      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation()
          onOpenCommandPalette?.()
        }}
        className={railButtonClass}
        title={t('activityRail.projects')}
        aria-label={t('activityRail.openProjects')}
        disabled={!onOpenCommandPalette}
      >
        <FolderKanban size={18} className="text-muted-foreground" />
      </button>

      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation()
          onOpenGitChanges?.()
        }}
        className={railButtonClass}
        title={
          canOpenGitChanges
            ? t('activityRail.gitChanges')
            : t('activityRail.gitChangesNeedsProject')
        }
        aria-label={t('activityRail.openGitChanges')}
        disabled={!onOpenGitChanges || !canOpenGitChanges}
      >
        <GitBranch
          size={18}
          className={canOpenGitChanges ? 'text-muted-foreground' : 'text-muted-foreground/40'}
        />
      </button>

      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation()
          onOpenAgentChat?.()
        }}
        className={railButtonClass}
        title={
          canOpenAgentChat
            ? t('activityRail.newAgentChat')
            : t('activityRail.newAgentChatNeedsProject')
        }
        aria-label={t('activityRail.newAgentChat')}
        disabled={!onOpenAgentChat || !canOpenAgentChat}
      >
        <MessageSquarePlus
          size={18}
          className={canOpenAgentChat ? 'text-muted-foreground' : 'text-muted-foreground/40'}
        />
      </button>

      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation()
          onOpenGitHistory?.()
        }}
        className={railButtonClass}
        title={
          canOpenGitHistory
            ? t('activityRail.gitHistory')
            : t('activityRail.gitHistoryNeedsProject')
        }
        aria-label={t('activityRail.openGitHistory')}
        disabled={!onOpenGitHistory || !canOpenGitHistory}
      >
        <History
          size={18}
          className={canOpenGitHistory ? 'text-muted-foreground' : 'text-muted-foreground/40'}
        />
      </button>

      <button
        type="button"
        onClick={(e) => {
          void handleToggleSSHPanel(e)
        }}
        className={railButtonClass}
        title={isTauriContext() ? t('activityRail.toggleSsh') : t('activityRail.sshDesktopOnly')}
        aria-label={isSSHPanelVisible ? t('activityRail.hideSsh') : t('activityRail.showSsh')}
        aria-pressed={isSSHPanelVisible}
        disabled={!isTauriContext()}
      >
        <Network
          size={18}
          className={
            isSSHPanelVisible
              ? 'text-foreground'
              : isTauriContext()
                ? 'text-muted-foreground'
                : 'text-muted-foreground/40'
          }
        />
      </button>

      <div className="mt-auto flex flex-col items-center pb-1">
        <TitleBarShortcutsPopover
          buttonClassName={railButtonClass}
          open={isShortcutsOpen}
          onOpenChange={onShortcutsOpenChange}
        />

        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation()
            navigate('/preferences')
          }}
          className={railButtonClass}
          title={t('activityRail.preferences')}
          aria-label={t('activityRail.openPreferences')}
          aria-current={location.pathname === '/preferences' ? 'page' : undefined}
        >
          <SlidersHorizontal
            size={18}
            className={
              location.pathname === '/preferences' ? 'text-foreground' : 'text-muted-foreground'
            }
          />
        </button>

        <button
          type="button"
          onClick={
            onToggleThemePicker
              ? (e) => {
                  e.stopPropagation()
                  onToggleThemePicker()
                }
              : undefined
          }
          className={railButtonClass}
          title={t('activityRail.colorThemes')}
          aria-label={t('activityRail.colorThemes')}
          aria-pressed={onToggleThemePicker ? isThemePickerOpen : undefined}
          aria-disabled={!onToggleThemePicker}
          disabled={!onToggleThemePicker}
        >
          <Palette
            size={18}
            className={isThemePickerOpen ? 'text-foreground' : 'text-muted-foreground'}
          />
        </button>
      </div>
    </nav>
  )
}
