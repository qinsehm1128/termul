import { AlertTriangle, MessageSquare } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'
import { ConversationLifecycleActions } from '@/components/chat/ChatHistoryEntryRow'
import { displayConversationTitle, sessionTitleForConversation } from '@/lib/conversation-title'
import { cn } from '@/lib/utils'
import { useAcpStore } from '@/stores/acp-store'
import {
  recoveryCountForConversation,
  useConversationStore,
  useVisibleConversations
} from '@/stores/conversation-store'
import { useProjectStore } from '@/stores/project-store'

const DEFAULT_PAGE_SIZE = 50

interface ConversationListProps {
  projectId?: string
  pageSize?: number
  onConversationOpened?: () => void
  className?: string
}

export function ConversationList({
  projectId,
  pageSize = DEFAULT_PAGE_SIZE,
  onConversationOpened,
  className
}: ConversationListProps): React.JSX.Element {
  const { t } = useTranslation('common')
  const navigate = useNavigate()
  const conversations = useVisibleConversations()
  const recoveryItems = useConversationStore((state) => state.recoveryItems)
  const activeConversationId = useConversationStore((state) => state.activeConversationId)
  const sessions = useAcpStore((state) => state.sessions)
  const sessionIndex = useAcpStore((state) => state.sessionIndex)
  const projects = useProjectStore((state) => state.projects)
  const [visibleCount, setVisibleCount] = useState(pageSize)

  const projected = useMemo(
    () =>
      projectId
        ? conversations.filter(
            (conversation) => conversation.projectAttachment?.projectId === projectId
          )
        : conversations,
    [conversations, projectId]
  )
  const visible = projected.slice(0, visibleCount)
  const hasMore = visible.length < projected.length

  if (projected.length === 0) {
    return (
      <div className="px-3 py-6 text-center text-xs text-muted-foreground" role="status">
        {t('conversationNavigation.empty')}
      </div>
    )
  }

  return (
    <div className={cn('flex flex-col py-1', className)} data-testid="conversation-list">
      {visible.map((conversation) => {
        const title = displayConversationTitle(conversation, {
          sessionTitle: sessionTitleForConversation(
            conversation.conversationId,
            sessions,
            sessionIndex
          ),
          untitled: t('conversationNavigation.untitled')
        })
        const project = conversation.projectAttachment
          ? projects.find((candidate) => candidate.id === conversation.projectAttachment?.projectId)
          : undefined
        const projectLabel = conversation.projectAttachment
          ? project?.name || conversation.projectAttachment.projectPathSnapshot
          : t('conversationNavigation.projectless')
        const recoveryCount = recoveryCountForConversation(
          recoveryItems,
          conversation.conversationId
        )
        const isActive = activeConversationId === conversation.conversationId
        return (
          <div
            key={conversation.conversationId}
            className={cn(
              'group flex min-h-11 items-center gap-1 border-l-2 pr-1 transition-colors',
              isActive
                ? 'border-primary bg-sidebar-accent'
                : 'border-transparent hover:bg-sidebar-accent/60'
            )}
            data-conversation-id={conversation.conversationId}
          >
            <button
              type="button"
              className="flex min-w-0 flex-1 items-center gap-2 px-3 py-2 text-left"
              aria-current={isActive ? 'page' : undefined}
              onClick={() => {
                navigate(`/c/${conversation.conversationId}`)
                onConversationOpened?.()
              }}
            >
              <MessageSquare className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-xs text-sidebar-foreground" title={title}>
                  {title}
                </span>
                <span className="block truncate text-[10px] text-muted-foreground">
                  {projectLabel}
                </span>
              </span>
              {recoveryCount > 0 && (
                <span
                  role="status"
                  className="inline-flex shrink-0 items-center gap-1 rounded-full bg-destructive/10 px-1.5 py-0.5 text-[10px] text-destructive"
                  aria-label={t('conversationNavigation.recoveryBadge', { count: recoveryCount })}
                >
                  <AlertTriangle className="size-3" aria-hidden="true" />
                  {recoveryCount}
                </span>
              )}
            </button>
            <ConversationLifecycleActions
              conversationId={conversation.conversationId}
              title={title}
            />
          </div>
        )
      })}
      {hasMore && (
        <button
          type="button"
          className="mx-2 min-h-9 rounded-md text-xs text-muted-foreground hover:bg-sidebar-accent"
          onClick={() => setVisibleCount((count) => count + pageSize)}
        >
          {t('conversationNavigation.loadMore', { count: projected.length - visible.length })}
        </button>
      )}
    </div>
  )
}
