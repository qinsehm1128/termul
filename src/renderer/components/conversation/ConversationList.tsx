import { AlertTriangle } from 'lucide-react'
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
  const { t: tConversation } = useTranslation('conversation')
  const navigate = useNavigate()
  const conversations = useVisibleConversations()
  const recoveryItems = useConversationStore((state) => state.recoveryItems)
  const activeConversationId = useConversationStore((state) => state.activeConversationId)
  const loadingList = useConversationStore((state) => state.loadingList)
  const listError = useConversationStore((state) => state.listError)
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
    if (listError) {
      return (
        <div className="px-3 py-5" role="alert">
          <p className="text-xs font-medium text-foreground">
            {t('conversationRoute.errors.title')}
          </p>
          <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{listError.message}</p>
        </div>
      )
    }

    if (loadingList) {
      return (
        <div className="flex flex-col gap-1 px-2 py-2" role="status" aria-busy="true">
          <span className="px-1 pb-1 text-xs text-muted-foreground">
            {tConversation('dashboard.loading')}
          </span>
          {Array.from({ length: 5 }, (_, index) => (
            <div key={index} className="flex h-8 items-center px-2">
              <span className="h-2.5 w-2/5 animate-pulse rounded-sm bg-muted" />
            </div>
          ))}
        </div>
      )
    }

    return (
      <div className="px-3 py-5" role="status">
        <p className="text-xs leading-relaxed text-muted-foreground">
          {t('conversationNavigation.empty')}
        </p>
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
              'group mx-1 flex min-h-8 items-center gap-0.5 rounded-sm pr-0.5 transition-colors',
              isActive
                ? 'bg-sidebar-accent text-foreground ring-1 ring-inset ring-primary/35'
                : 'hover:bg-sidebar-accent/50'
            )}
            data-conversation-id={conversation.conversationId}
          >
            <button
              type="button"
              className="flex min-h-8 min-w-0 flex-1 items-center gap-2 px-2 py-1.5 text-left focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring"
              aria-current={isActive ? 'page' : undefined}
              onClick={() => {
                navigate(`/c/${conversation.conversationId}`)
                onConversationOpened?.()
              }}
            >
              <span className="min-w-0 flex-1">
                <span
                  className={cn(
                    'block truncate text-xs leading-4',
                    isActive ? 'font-medium text-foreground' : 'text-sidebar-foreground'
                  )}
                  title={title}
                >
                  {title}
                </span>
                <span className="block truncate text-2xs leading-4 text-muted-foreground">
                  {projectLabel}
                </span>
              </span>
              {recoveryCount > 0 && (
                <span
                  role="status"
                  className="inline-flex shrink-0 items-center gap-0.5 text-2xs tabular-nums text-destructive"
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
          className="mx-1 mt-0.5 inline-flex h-8 items-center justify-center rounded-sm px-2 text-xs text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          onClick={() => setVisibleCount((count) => count + pageSize)}
        >
          {t('conversationNavigation.loadMore', { count: projected.length - visible.length })}
        </button>
      )}
    </div>
  )
}
