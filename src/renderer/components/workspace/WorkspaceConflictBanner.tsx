import type { ConversationId } from '@shared/types/conversation.types'
import type { RecoveryActionName, RecoveryItemV1 } from '@shared/types/conversation-recovery.types'
import { useCallback } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import {
  resolveSessionWorkspaceConflict,
  resolveSessionWorkspaceRecovery
} from '@/hooks/use-session-workspace-sync'
import { useSessionWorkspaceSyncStore } from '@/stores/session-workspace-sync-store'

export interface WorkspaceConflictBannerProps {
  conversationId?: ConversationId | null
}

const recoveryActions: readonly RecoveryActionName[] = [
  'inspect',
  'associateConversation',
  'startEmptyWorkspace',
  'dismissPreservedSource'
]

function RecoveryItemActions({
  conversationId,
  item
}: {
  conversationId: ConversationId
  item: RecoveryItemV1
}): React.ReactElement {
  const run = useCallback(
    (action: RecoveryActionName) => {
      void resolveSessionWorkspaceRecovery(conversationId, item, action)
    },
    [conversationId, item]
  )
  return (
    <section className="flex min-w-0 flex-col gap-2" aria-label={item.kind}>
      <div className="min-w-0 text-xs text-muted-foreground">
        <div className="font-medium text-amber-700 dark:text-amber-300">{item.kind}</div>
        {item.sourcePaths.map((path, index) => (
          <div key={`${path}-${item.sourceSha256[index] ?? ''}`} className="break-all font-mono">
            <span>{path}</span>
            {item.sourceSha256[index] ? <span> · sha256:{item.sourceSha256[index]}</span> : null}
          </div>
        ))}
        <div>revision {item.revision}</div>
      </div>
      <div className="flex flex-wrap gap-2">
        {recoveryActions.map((action) => (
          <Button
            key={action}
            type="button"
            variant="secondary"
            size="xs"
            onClick={() => run(action)}
          >
            {action}
          </Button>
        ))}
      </div>
    </section>
  )
}

export function WorkspaceConflictBanner({
  conversationId: conversationIdProp
}: WorkspaceConflictBannerProps = {}): null | React.ReactElement {
  const { t } = useTranslation('workspace')
  const storeConversationId = useSessionWorkspaceSyncStore((state) => state.activeConversationId)
  const conversationId = conversationIdProp ?? storeConversationId
  const conflict = useSessionWorkspaceSyncStore((state) =>
    conversationId ? state.conflictsByConversation[conversationId] : undefined
  )
  const recoveryItemsValue = useSessionWorkspaceSyncStore((state) =>
    conversationId ? state.recoveryByConversation[conversationId] : undefined
  )
  const recoveryItems = recoveryItemsValue ?? []

  const resolveConflict = useCallback(
    (action: 'reload' | 'overwrite' | 'dismiss') => {
      if (conversationId) void resolveSessionWorkspaceConflict(conversationId, action)
    },
    [conversationId]
  )

  if (!conversationId || (!conflict && recoveryItems.length === 0)) return null

  return (
    <div
      role="alert"
      aria-live="polite"
      data-conversation-id={conversationId}
      className="flex max-h-[40vh] flex-col gap-3 overflow-auto border-b border-amber-500/50 bg-amber-500/10 px-3 py-2"
    >
      {conflict ? (
        <section className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <div className="text-sm font-medium text-amber-700 dark:text-amber-300">
              {t('conflict.title')}
            </div>
            <div className="break-words text-xs text-muted-foreground">
              Conversation {conversationId} · revision {conflict.currentRevision}
              {conflict.currentUpdatedAtUtc ? ` · ${conflict.currentUpdatedAtUtc}` : ''}
              {conflict.currentUpdateIdentity ? ` · ${conflict.currentUpdateIdentity}` : ''}
            </div>
          </div>
          <div className="flex shrink-0 flex-wrap gap-2">
            <Button
              type="button"
              variant="default"
              size="xs"
              onClick={() => resolveConflict('reload')}
            >
              {t('conflict.reload')}
            </Button>
            <Button
              type="button"
              variant="secondary"
              size="xs"
              onClick={() => resolveConflict('overwrite')}
            >
              {t('conflict.overwrite')}
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={() => resolveConflict('dismiss')}
            >
              {t('conflict.dismiss')}
            </Button>
          </div>
        </section>
      ) : null}
      {recoveryItems.map((item) => (
        <RecoveryItemActions key={item.recoveryId} conversationId={conversationId} item={item} />
      ))}
    </div>
  )
}
