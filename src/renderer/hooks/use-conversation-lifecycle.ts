import { useEffect } from 'react'
import { conversationLifecycleApi } from '@/lib/conversation-lifecycle-api'
import { useAcpStore } from '@/stores/acp-store'

/** Reconcile host lifecycle outcomes in either renderer root without owning the mutation itself. */
export function useConversationLifecycle(): void {
  useEffect(
    () =>
      conversationLifecycleApi.subscribe((outcome) => {
        useAcpStore.getState()._onConversationLifecycle(outcome)
      }),
    []
  )
}
