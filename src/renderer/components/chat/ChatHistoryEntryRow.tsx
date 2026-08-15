import type { ConversationLifecycleOutcome } from '@shared/types/conversation-lifecycle.types'
import { Link2, MoreHorizontal, PauseCircle, RefreshCw, Trash2, Unlink, X } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { ConfirmDialog } from '@/components/ConfirmDialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger
} from '@/components/ui/dropdown-menu'
import { cn } from '@/lib/utils'
import { useAcpStore, useAgentTemplateId } from '@/stores/acp-store'
import { AgentGlyph } from './AgentGlyph'

export interface ChatHistorySidebarEntry {
  id: string
  conversationId?: string
  title: string
  messageCount: number
  status: string
  discovered: boolean
  agentId?: string
  agentConfigId?: string
  agentName?: string | null
  cwd?: string
  lastActivityAt: number
  canOpen: boolean
}

type ConfirmedLifecycleAction = 'detach' | 'rebind' | 'suspend' | 'replace' | 'delete'

/** Resolve the agent's bundled registry icon for a history/discovered entry. */
function ChatEntryIcon({
  agentId,
  agentConfigId
}: {
  agentId?: string
  agentConfigId?: string
}): React.JSX.Element {
  const templateId = useAgentTemplateId(agentId ?? null, agentConfigId)
  return <AgentGlyph templateId={templateId} size={12} className="text-muted-foreground" />
}

function lifecycleErrorCode(error: unknown): string {
  if (error && typeof error === 'object' && 'code' in error && typeof error.code === 'string') {
    return error.code
  }
  return 'CONVERSATION_RECOVERY_REQUIRED'
}

function blockedResourceSummary(outcome: ConversationLifecycleOutcome): string {
  if (outcome.status !== 'blocked') return ''
  return outcome.blockers
    .flatMap((blocker) => blocker.ids)
    .filter(Boolean)
    .join(', ')
}

export interface ConversationLifecycleActionsProps {
  conversationId?: string
  title: string
  onViewClosed?: () => void
  className?: string
}

/**
 * Accessible, transport-neutral Conversation lifecycle menu shared by desktop project/history
 * lists and the mobile chat shell. Every destructive host action is confirmed independently;
 * close-view stays renderer-local and immediate.
 */
export function ConversationLifecycleActions({
  conversationId,
  title,
  onViewClosed,
  className
}: ConversationLifecycleActionsProps): React.JSX.Element {
  const { t } = useTranslation('chat')
  const closeChatView = useAcpStore((state) => state.closeChatView)
  const detachAgentBinding = useAcpStore((state) => state.detachAgentBinding)
  const rebindDetachedBinding = useAcpStore((state) => state.rebindDetachedBinding)
  const suspendAgentBinding = useAcpStore((state) => state.suspendAgentBinding)
  const replaceAgentBinding = useAcpStore((state) => state.replaceAgentBinding)
  const deleteConversation = useAcpStore((state) => state.deleteConversation)
  const [pendingAction, setPendingAction] = useState<ConfirmedLifecycleAction | null>(null)
  const [running, setRunning] = useState(false)

  const closeView = (): void => {
    if (!conversationId) return
    closeChatView(conversationId)
    onViewClosed?.()
  }

  const runConfirmedAction = async (): Promise<void> => {
    if (!conversationId || !pendingAction) return
    const action = pendingAction
    setPendingAction(null)
    setRunning(true)
    try {
      const outcome =
        action === 'detach'
          ? await detachAgentBinding(conversationId)
          : action === 'rebind'
            ? await rebindDetachedBinding(conversationId)
            : action === 'suspend'
              ? await suspendAgentBinding(conversationId)
              : action === 'replace'
                ? await replaceAgentBinding(conversationId)
                : await deleteConversation(conversationId)
      if (outcome.status === 'blocked') {
        toast.error(t('lifecycle.blocked.title'), {
          description: t('lifecycle.blocked.description', {
            resources: blockedResourceSummary(outcome)
          })
        })
        return
      }
      toast.success(t(`lifecycle.success.${action}`))
    } catch (error) {
      const code = lifecycleErrorCode(error)
      toast.error(t(`lifecycle.errors.${code}`, { defaultValue: code }))
    } finally {
      setRunning(false)
    }
  }

  const disabled = !conversationId || running
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            disabled={disabled}
            aria-label={t('lifecycle.actionsFor', { title })}
            title={conversationId ? t('lifecycle.actions') : t('lifecycle.legacyReadOnly')}
            className={cn(
              'relative inline-flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground',
              "after:absolute after:-inset-1.5 after:content-['']",
              'transition-colors hover:bg-background/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-40',
              'pointer-fine:opacity-0 pointer-fine:group-hover:opacity-100 focus-visible:opacity-100',
              className
            )}
          >
            <MoreHorizontal size={14} aria-hidden="true" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-56">
          <DropdownMenuItem onSelect={closeView}>
            <X className="mr-2 size-4" aria-hidden="true" />
            {t('lifecycle.closeView')}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => setPendingAction('detach')}>
            <Unlink className="mr-2 size-4" aria-hidden="true" />
            {t('lifecycle.detach')}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setPendingAction('rebind')}>
            <Link2 className="mr-2 size-4" aria-hidden="true" />
            {t('lifecycle.rebind')}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setPendingAction('suspend')}>
            <PauseCircle className="mr-2 size-4" aria-hidden="true" />
            {t('lifecycle.suspend')}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setPendingAction('replace')}>
            <RefreshCw className="mr-2 size-4" aria-hidden="true" />
            {t('lifecycle.replace')}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            className="text-destructive focus:text-destructive"
            onSelect={() => setPendingAction('delete')}
          >
            <Trash2 className="mr-2 size-4" aria-hidden="true" />
            {t('lifecycle.delete')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <ConfirmDialog
        isOpen={pendingAction !== null}
        title={pendingAction ? t(`lifecycle.confirm.${pendingAction}.title`) : ''}
        message={
          pendingAction
            ? t(`lifecycle.confirm.${pendingAction}.message`, {
                title
              })
            : ''
        }
        confirmLabel={pendingAction ? t(`lifecycle.confirm.${pendingAction}.confirm`) : ''}
        cancelLabel={t('common.cancel')}
        variant={pendingAction === 'delete' ? 'danger' : 'default'}
        onConfirm={() => void runConfirmedAction()}
        onCancel={() => setPendingAction(null)}
      />
    </>
  )
}

interface ChatHistoryEntryRowProps {
  entry: ChatHistorySidebarEntry
  onOpen: (entry: ChatHistorySidebarEntry) => void
  onViewClosed?: () => void
}

export function ChatHistoryEntryRow({
  entry,
  onOpen,
  onViewClosed
}: ChatHistoryEntryRowProps): React.JSX.Element {
  const { t } = useTranslation('chat')
  return (
    <div
      className={cn(
        'group flex w-full items-center gap-2 pr-2 hover:bg-sidebar-accent',
        entry.status === 'closed' && 'opacity-70',
        entry.discovered && !entry.canOpen && 'opacity-50'
      )}
    >
      <button
        type="button"
        disabled={entry.discovered && !entry.canOpen}
        onClick={() => onOpen(entry)}
        title={
          entry.discovered && !entry.canOpen
            ? t('history.unsupported')
            : entry.discovered && entry.agentName
              ? t('history.resumeTitle', { title: entry.title, agent: entry.agentName })
              : entry.title
        }
        className="flex min-w-0 flex-1 items-center gap-2 px-3 py-1.5 text-left text-xs disabled:cursor-not-allowed"
      >
        <ChatEntryIcon agentId={entry.agentId} agentConfigId={entry.agentConfigId} />
        <span className="truncate flex-1 text-sidebar-foreground">{entry.title}</span>
        {entry.discovered ? (
          entry.agentName ? (
            <span className="text-3xs text-muted-foreground/70 shrink-0">{entry.agentName}</span>
          ) : null
        ) : (
          <span className="text-3xs text-muted-foreground">{entry.messageCount}</span>
        )}
      </button>
      {!entry.discovered && (
        <ConversationLifecycleActions
          conversationId={entry.conversationId}
          title={entry.title}
          onViewClosed={onViewClosed}
        />
      )}
    </div>
  )
}
