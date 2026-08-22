import type { CliSessionAgentId, DiscoveredCliSession } from '@shared/types/cli-session.types'
import { CLI_SESSION_AGENT_IDS, CLI_SESSION_AGENT_LABELS } from '@shared/types/cli-session.types'
import { History, RefreshCw } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { CliSessionResumeDialog } from '@/components/cli-sessions/CliSessionResumeDialog'
import { launchAgentResumeInPane } from '@/lib/agent-launch'
import { getBuiltInAgent } from '@/lib/agents/agent-registry'
import { cliSessionApi } from '@/lib/api'
import { loadCliResumeDefaults } from '@/lib/cli-resume-defaults'
import { buildCliSessionScopePaths, type CliSessionScopeMode } from '@/lib/cli-session-scope'
import { logFrontendError } from '@/lib/log-api'
import { getDefaultCwdForProject } from '@/lib/worktree-context'
import { useCliSessionPanelVisible } from '@/stores/cli-session-panel-store'
import { useActiveProject } from '@/stores/project-store'
import { useActiveTerminal } from '@/stores/terminal-store'
import { useWorkspaceStore } from '@/stores/workspace-store'

interface CliSessionPanelProps {
  className?: string
  /** Ignore the persisted panel store (mobile sheet). */
  forceVisible?: boolean
}

export function CliSessionPanel({
  className,
  forceVisible = false
}: CliSessionPanelProps): React.JSX.Element | null {
  const storeVisible = useCliSessionPanelVisible()
  const isVisible = forceVisible || storeVisible
  const activeProject = useActiveProject()
  const activeTerminal = useActiveTerminal()
  const [scopeMode, setScopeMode] = useState<CliSessionScopeMode>('directory')
  const [agentFilter, setAgentFilter] = useState<'all' | CliSessionAgentId>('all')
  const [sessions, setSessions] = useState<DiscoveredCliSession[]>([])
  const [issues, setIssues] = useState<number>(0)
  const [loading, setLoading] = useState(false)
  const [resumeSession, setResumeSession] = useState<DiscoveredCliSession | null>(null)
  const [resumeBusy, setResumeBusy] = useState(false)
  const [defaultsByAgent, setDefaultsByAgent] = useState<
    Partial<Record<CliSessionAgentId, string>>
  >({})

  const directoryPath =
    activeTerminal?.cwd ?? (activeProject ? getDefaultCwdForProject(activeProject.id) : null)
  const scopePaths = useMemo(
    () =>
      buildCliSessionScopePaths({
        mode: scopeMode,
        directoryPath,
        projectPath: activeProject?.path,
        worktreePaths: activeProject?.worktrees?.map((worktree) => worktree.path)
      }),
    [scopeMode, directoryPath, activeProject?.path, activeProject?.worktrees]
  )

  const refresh = useCallback(async (): Promise<void> => {
    setLoading(true)
    try {
      const result = await cliSessionApi.listSessions({
        scopePaths,
        agents: agentFilter === 'all' ? undefined : [agentFilter]
      })
      setSessions(result.sessions)
      setIssues(result.issues.length)
      if (result.issues.length > 0) {
        void logFrontendError({
          level: 'warn',
          source: 'CliSessionPanel',
          message: `cli session scan issues=${result.issues.length}`
        })
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      toast.error(message)
      void logFrontendError({
        source: 'CliSessionPanel',
        message
      })
    } finally {
      setLoading(false)
    }
  }, [agentFilter, scopePaths])

  useEffect(() => {
    if (!isVisible) return
    void loadCliResumeDefaults().then((defaults) => {
      setDefaultsByAgent(defaults.extraArgsByAgentId)
    })
    void refresh()
  }, [isVisible, refresh])

  const grouped = useMemo(() => {
    const groups = new Map<CliSessionAgentId, DiscoveredCliSession[]>()
    for (const session of sessions) {
      const list = groups.get(session.agentId) ?? []
      list.push(session)
      groups.set(session.agentId, list)
    }
    return CLI_SESSION_AGENT_IDS.filter((agentId) => groups.has(agentId)).map((agentId) => ({
      agentId,
      sessions: groups.get(agentId) ?? []
    }))
  }, [sessions])

  const handleResume = async (onceExtraArgs: string): Promise<void> => {
    if (!resumeSession || !activeProject) return
    const def = getBuiltInAgent(resumeSession.agentId)
    const paneId = useWorkspaceStore.getState().activePaneId
    if (!def || !paneId) {
      toast.error('No active pane or agent definition')
      return
    }
    setResumeBusy(true)
    try {
      const cwd = resumeSession.cwd || directoryPath || activeProject.path || ''
      const result = await launchAgentResumeInPane(
        paneId,
        activeProject.id,
        cwd,
        def,
        resumeSession,
        defaultsByAgent[resumeSession.agentId] ?? '',
        onceExtraArgs
      )
      if (!result.success) {
        toast.error(result.error || 'Failed to resume session')
        void logFrontendError({
          source: 'CliSessionPanel',
          message: `resume failed agent=${resumeSession.agentId} session=${resumeSession.sessionId}`
        })
        return
      }
      setResumeSession(null)
    } finally {
      setResumeBusy(false)
    }
  }

  if (!isVisible) return null

  return (
    <div
      className={
        className ??
        'relative flex h-full w-[280px] min-w-0 flex-shrink-0 flex-col overflow-hidden rounded-xl bg-background text-foreground'
      }
      data-testid="cli-session-panel"
    >
      <div className="flex items-center justify-between px-3 h-10 border-b border-border flex-shrink-0 rounded-t-xl">
        <span className="flex items-center gap-1.5 text-xs tracking-wider text-sidebar-foreground uppercase">
          <History size={12} />
          CLI Sessions
        </span>
        <button
          type="button"
          onClick={() => void refresh()}
          disabled={loading}
          className="text-muted-foreground hover:text-foreground transition-colors p-0.5 rounded-sm focus:outline-none focus-visible:ring-1 focus-visible:ring-primary disabled:opacity-40"
          title="Refresh"
          aria-label="Refresh CLI sessions"
        >
          <RefreshCw size={14} className={loading ? 'animate-spin' : undefined} />
        </button>
      </div>

      <div className="flex flex-col gap-2 border-b border-border p-2">
        <label className="flex flex-col gap-1 text-xs text-muted-foreground">
          Scope
          <select
            className="bg-secondary/50 border border-border rounded-md px-2 py-1 text-xs text-foreground"
            value={scopeMode}
            onChange={(event) => setScopeMode(event.target.value as CliSessionScopeMode)}
          >
            <option value="directory">This directory</option>
            <option value="project">This project</option>
            <option value="all">All sessions</option>
          </select>
        </label>
        <label className="flex flex-col gap-1 text-xs text-muted-foreground">
          Agent
          <select
            className="bg-secondary/50 border border-border rounded-md px-2 py-1 text-xs text-foreground"
            value={agentFilter}
            onChange={(event) => setAgentFilter(event.target.value as 'all' | CliSessionAgentId)}
          >
            <option value="all">All agents</option>
            {CLI_SESSION_AGENT_IDS.map((agentId) => (
              <option key={agentId} value={agentId}>
                {CLI_SESSION_AGENT_LABELS[agentId]}
              </option>
            ))}
          </select>
        </label>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {grouped.length === 0 && !loading ? (
          <p className="px-3 py-4 text-xs text-muted-foreground">
            No CLI sessions found for this scope.
          </p>
        ) : (
          grouped.map((group) => (
            <section key={group.agentId} className="border-b border-border/60">
              <h3 className="px-3 py-2 text-2xs uppercase tracking-wider text-muted-foreground">
                {CLI_SESSION_AGENT_LABELS[group.agentId]}
              </h3>
              <ul>
                {group.sessions.map((session) => (
                  <li key={session.id}>
                    <button
                      type="button"
                      disabled={!session.resumable}
                      onClick={() => setResumeSession(session)}
                      className="w-full px-3 py-2 text-left hover:bg-secondary/60 disabled:opacity-40 disabled:cursor-not-allowed"
                    >
                      <div className="truncate text-sm text-foreground">{session.title}</div>
                      <div className="truncate text-2xs text-muted-foreground">
                        {session.cwd ?? 'Unknown cwd'}
                        {session.updatedAt ? ` · ${session.updatedAt.slice(0, 10)}` : ''}
                      </div>
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          ))
        )}
      </div>

      {issues > 0 ? (
        <div className="px-3 py-2 text-2xs text-muted-foreground border-t border-border">
          {issues} scan issue{issues === 1 ? '' : 's'} (see logs)
        </div>
      ) : null}

      <CliSessionResumeDialog
        session={resumeSession}
        defaultExtraArgs={resumeSession ? (defaultsByAgent[resumeSession.agentId] ?? '') : ''}
        busy={resumeBusy}
        onOpenChange={(open) => {
          if (!open) setResumeSession(null)
        }}
        onConfirm={(onceExtraArgs) => {
          void handleResume(onceExtraArgs)
        }}
      />
    </div>
  )
}
