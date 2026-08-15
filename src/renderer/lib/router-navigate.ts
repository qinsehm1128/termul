let navigateFn: ((path: string) => void) | null = null

export function setRouterNavigate(fn: ((path: string) => void) | null): void {
  navigateFn = fn
}

export function navigateToConversation(conversationId: string): void {
  if (!navigateFn) return
  const target = `/c/${encodeURIComponent(conversationId)}`
  if (window.location.hash !== `#${target}`) navigateFn(target)
}

export function navigateToChatSession(sessionId: string): void {
  if (!navigateFn) return
  const target = `/legacy/session/${encodeURIComponent(sessionId)}`
  if (window.location.hash !== `#${target}`) {
    navigateFn(target)
  }
}

export function clearChatRoute(): void {
  if (!navigateFn) return
  if (window.location.hash.startsWith('#/c/') || window.location.hash.startsWith('#/legacy/')) {
    navigateFn('/')
  }
}
