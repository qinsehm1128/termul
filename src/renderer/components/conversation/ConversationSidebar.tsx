import { MessageSquarePlus, Search, X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { ConversationList } from '@/components/conversation/ConversationList'
import { useConversationStore } from '@/stores/conversation-store'
import { useProjectStore } from '@/stores/project-store'

interface ConversationSidebarProps {
  onNewChat?: () => void
}

export function ConversationSidebar({ onNewChat }: ConversationSidebarProps): React.JSX.Element {
  const { t } = useTranslation('common')
  const searchQuery = useConversationStore((state) => state.searchQuery)
  const projectFilter = useConversationStore((state) => state.projectFilter)
  const setSearchQuery = useConversationStore((state) => state.setSearchQuery)
  const setProjectFilter = useConversationStore((state) => state.setProjectFilter)
  const projects = useProjectStore((state) => state.projects)

  return (
    <aside className="flex h-full w-60 shrink-0 flex-col bg-sidebar">
      <div className="flex h-8 items-center justify-between border-b border-sidebar-border/70 px-2.5">
        <span className="label-section text-sidebar-foreground">
          {t('conversationNavigation.title')}
        </span>
        <button
          type="button"
          className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors duration-150 ease-[var(--ease-out)] hover:bg-sidebar-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          aria-label={t('conversationNavigation.newChat')}
          title={t('conversationNavigation.newChat')}
          onClick={onNewChat}
        >
          <MessageSquarePlus className="size-3.5" aria-hidden="true" />
        </button>
      </div>

      <div className="space-y-1.5 border-b border-sidebar-border/70 px-2.5 py-1.5">
        <div className="relative">
          <Search
            className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground"
            aria-hidden="true"
          />
          <input
            type="search"
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Escape' && searchQuery) {
                event.preventDefault()
                setSearchQuery('')
              }
            }}
            placeholder={t('conversationNavigation.search')}
            aria-label={t('conversationNavigation.search')}
            className="h-8 w-full rounded-none border-0 bg-transparent pl-7 pr-7 text-xs text-foreground outline-none placeholder:text-muted-foreground/60 focus-visible:ring-1 focus-visible:ring-ring/50 [&::-webkit-search-cancel-button]:hidden"
          />
          {searchQuery && (
            <button
              type="button"
              className="absolute right-0 top-1/2 inline-flex size-7 -translate-y-1/2 items-center justify-center rounded-md text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              aria-label={t('conversationNavigation.clearSearch')}
              onClick={() => setSearchQuery('')}
            >
              <X className="size-3.5" aria-hidden="true" />
            </button>
          )}
        </div>

        <label className="block">
          <span className="sr-only">{t('conversationNavigation.filter')}</span>
          <select
            value={projectFilter ?? 'all'}
            onChange={(event) =>
              setProjectFilter(event.target.value === 'all' ? null : event.target.value)
            }
            aria-label={t('conversationNavigation.filter')}
            className="h-8 w-full rounded-md border-0 bg-secondary/35 px-2 text-xs text-foreground outline-none focus-visible:ring-1 focus-visible:ring-ring/50"
          >
            <option value="all">{t('conversationNavigation.allProjects')}</option>
            <option value="projectless">{t('conversationNavigation.projectless')}</option>
            {projects
              .filter((project) => !project.isArchived)
              .map((project) => (
                <option key={project.id} value={project.id}>
                  {project.name}
                </option>
              ))}
          </select>
        </label>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <ConversationList />
      </div>
    </aside>
  )
}
