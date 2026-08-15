import type { ConversationApi } from '@shared/types/conversation-api.types'
import { createTauriConversationApi } from './tauri-conversation-api'
import { isTauriContext } from './tauri-runtime'
import { createWebConversationApi } from './web-conversation-api'

export const conversationApi: ConversationApi = isTauriContext()
  ? createTauriConversationApi()
  : createWebConversationApi()

export { createTauriConversationApi } from './tauri-conversation-api'
export { createWebConversationApi, webConversationApi } from './web-conversation-api'
