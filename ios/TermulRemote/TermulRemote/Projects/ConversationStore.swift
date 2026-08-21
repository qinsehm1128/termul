import Foundation
import Observation

@MainActor
@Observable
final class ConversationStore {
    var conversations: [HostConversation] = []
    var active: HostConversation?
    var errorMessage: String?

    private var http: HostHTTP?

    func attach(http: HostHTTP) {
        self.http = http
    }

    func refresh() async {
        guard let http else { return }
        do {
            let listed: [HostConversation] = try await http.get("conversations")
            conversations = listed.filter { !$0.isDeleted }
            if let current = active {
                active = conversations.first(where: { $0.id == current.id })
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func select(_ conversation: HostConversation) {
        active = conversation
    }

    func clearSelection() {
        active = nil
    }

    func open(_ conversation: HostConversation) async -> ConversationOpenOutcome? {
        guard let http else { return nil }
        do {
            let opened: ConversationOpenOutcome = try await http.post(
                "conversations/\(conversation.id)/open",
                body: [:]
            )
            if let record = opened.conversation {
                select(record)
            } else {
                select(conversation)
            }
            return opened
        } catch {
            errorMessage = error.localizedDescription
            select(conversation)
            return nil
        }
    }
}
