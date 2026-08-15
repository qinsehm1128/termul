import type { ConversationId } from '@shared/types/conversation.types'
import { AlertTriangle, LoaderCircle } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router-dom'
import { loadSessionWorkspace } from '@/hooks/use-session-workspace-sync'
import { logFrontendError } from '@/lib/log-api'
import { useAcpStore } from '@/stores/acp-store'
import { isCanonicalConversationId, useConversationStore } from '@/stores/conversation-store'
import { useSessionWorkspaceSyncStore } from '@/stores/session-workspace-sync-store'
import { useWorkspaceStore } from '@/stores/workspace-store'

interface ConversationRouteProps {
  conversationId?: string
}

const routeErrorKeyByCode = {
  CONVERSATION_INVALID_ID: 'conversationRoute.errors.CONVERSATION_INVALID_ID',
  CONVERSATION_NOT_FOUND: 'conversationRoute.errors.CONVERSATION_NOT_FOUND',
  LEGACY_ID_AMBIGUOUS: 'conversationRoute.errors.LEGACY_ID_AMBIGUOUS',
  CONVERSATION_RECOVERY_REQUIRED: 'conversationRoute.errors.CONVERSATION_RECOVERY_REQUIRED',
  CONVERSATION_BINDING_OPEN_FAILED: 'conversationRoute.errors.CONVERSATION_BINDING_OPEN_FAILED',
  CONVERSATION_LEGACY_RESOLVE_FAILED: 'conversationRoute.errors.CONVERSATION_LEGACY_RESOLVE_FAILED',
  CONVERSATION_OPEN_FAILED: 'conversationRoute.errors.CONVERSATION_OPEN_FAILED'
} as const

export function conversationRouteErrorKey(
  code: string
): (typeof routeErrorKeyByCode)[keyof typeof routeErrorKeyByCode] {
  return (
    routeErrorKeyByCode[code as keyof typeof routeErrorKeyByCode] ??
    routeErrorKeyByCode.CONVERSATION_OPEN_FAILED
  )
}

function bindingSessionId(conversationId: ConversationId): string | null {
  const acp = useAcpStore.getState()
  const live = Object.values(acp.sessions).find(
    (session) => session.conversationId === conversationId
  )
  if (live) return live.id
  return acp.sessionIndex.find((entry) => entry.conversationId === conversationId)?.id ?? null
}

export function ConversationRoute({
  conversationId: conversationIdProp
}: ConversationRouteProps = {}): React.JSX.Element | null {
  const { t } = useTranslation('common')
  const params = useParams<{ conversationId: string }>()
  const routeValue = conversationIdProp ?? params.conversationId ?? ''
  const openConversation = useConversationStore((state) => state.openConversation)
  const opening = useConversationStore((state) => state.openingById[routeValue] === true)
  const storeError = useConversationStore((state) => state.errorsById[routeValue])
  const [bindingErrorCode, setBindingErrorCode] = useState<string | null>(null)

  const open = useCallback(async (): Promise<void> => {
    setBindingErrorCode(null)
    const outcome = await openConversation(routeValue)
    if (!outcome || !isCanonicalConversationId(routeValue)) return

    useSessionWorkspaceSyncStore.getState().setActiveConversationId(routeValue)
    await loadSessionWorkspace(routeValue)

    const sessionId = bindingSessionId(routeValue)
    if (!sessionId) return
    try {
      const acp = useAcpStore.getState()
      const live = acp.sessions[sessionId]
      if (!live || live.status === 'closed') await acp.openHistorySession(sessionId)
      acp.setActiveSession(sessionId)
      useWorkspaceStore.getState().addAgentChatTab(sessionId, undefined, false)
    } catch {
      setBindingErrorCode('CONVERSATION_BINDING_OPEN_FAILED')
      void logFrontendError({
        level: 'warn',
        source: 'conversation-route.binding',
        message: `conversationId=${routeValue} code=CONVERSATION_BINDING_OPEN_FAILED`
      })
    }
  }, [openConversation, routeValue])

  useEffect(() => {
    void open()
  }, [open])

  const errorCode = storeError?.code ?? bindingErrorCode
  if (opening && !errorCode) {
    return (
      <div
        role="status"
        aria-live="polite"
        className="absolute inset-x-3 top-3 z-20 flex items-center gap-2 rounded-md border bg-background/95 px-3 py-2 text-sm shadow-sm"
      >
        <LoaderCircle className="size-4 animate-spin" aria-hidden="true" />
        {t('conversationRoute.opening')}
      </div>
    )
  }

  if (!errorCode) return null

  const canRetry = errorCode !== 'CONVERSATION_INVALID_ID' && errorCode !== 'CONVERSATION_NOT_FOUND'
  return (
    <div
      role="alert"
      aria-live="assertive"
      className="absolute inset-x-3 top-3 z-20 rounded-md border border-destructive bg-background/95 p-3 text-sm text-destructive shadow-sm"
      data-error-code={errorCode}
    >
      <div className="flex items-start gap-2">
        <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        <div className="min-w-0 flex-1">
          <p className="font-medium">{t('conversationRoute.errors.title')}</p>
          <p className="mt-1 text-xs">{t(conversationRouteErrorKey(errorCode))}</p>
          <code className="mt-1 block break-all font-mono text-[10px] opacity-80">{errorCode}</code>
        </div>
      </div>
      {canRetry && (
        <button
          type="button"
          className="mt-3 min-h-9 rounded-md border border-destructive/40 px-3 text-xs font-medium hover:bg-destructive/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-destructive"
          onClick={() => void open()}
        >
          {t('actions.retry')}
        </button>
      )}
    </div>
  )
}
