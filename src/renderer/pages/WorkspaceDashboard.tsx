import { MessageSquarePlus } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { ConversationList } from '@/components/conversation/ConversationList'
import { useConversationStore } from '@/stores/conversation-store'

export default function WorkspaceDashboard(): React.JSX.Element {
  const { t } = useTranslation('conversation')
  const loading = useConversationStore((state) => state.loadingList)
  const conversationIds = useConversationStore((state) => state.conversationIds)
  const activeConversationId = useConversationStore((state) => state.activeConversationId)
  const activeConversation = useConversationStore((state) =>
    activeConversationId ? state.summariesById[activeConversationId] : undefined
  )

  return (
    <section
      className="flex h-full min-h-0 flex-col bg-background p-4"
      aria-labelledby="conversation-dashboard-title"
    >
      <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col gap-4">
        <header className="rounded-xl border border-border bg-card p-5">
          <MessageSquarePlus className="mb-3 size-8 text-primary" aria-hidden="true" />
          <h1 id="conversation-dashboard-title" className="text-xl font-semibold">
            {t('dashboard.title')}
          </h1>
          <p className="mt-1 text-sm text-muted-foreground">{t('dashboard.description')}</p>
          {activeConversation ? (
            <div className="mt-4 rounded-lg bg-muted/50 p-3 text-xs">
              <span className="font-medium">{t('dashboard.workspace')}:</span>{' '}
              <code className="break-all font-mono">{activeConversation.workspaceCwd}</code>
            </div>
          ) : null}
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto rounded-xl border border-border bg-card">
          {loading ? (
            <div role="status" className="p-5 text-sm text-muted-foreground">
              {t('dashboard.loading')}
            </div>
          ) : conversationIds.length === 0 ? (
            <div className="p-5 text-sm text-muted-foreground">{t('dashboard.empty')}</div>
          ) : (
            <ConversationList />
          )}
        </div>
      </div>
    </section>
  )
}
