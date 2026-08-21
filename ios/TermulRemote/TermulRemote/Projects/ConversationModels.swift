import Foundation

struct HostConversation: Identifiable, Hashable, Decodable, Sendable {
    var conversationId: String
    var title: String?
    var workspaceCwd: String
    var lifecycleState: String
    var lastSeq: UInt64?
    var projectAttachment: ConversationProjectAttachment?
    var executionTarget: ConversationExecutionTarget?

    var id: String { conversationId }

    var displayTitle: String {
        if let title, !title.isEmpty { return title }
        let name = workspaceCwd.split(separator: "/").last.map(String.init)
        return name?.isEmpty == false ? name! : conversationId
    }

    var projectId: String? {
        projectAttachment?.projectId ?? executionTarget?.projectId
    }

    var isDeleted: Bool { lifecycleState == "deleted" }
}

struct ConversationProjectAttachment: Hashable, Decodable, Sendable {
    var projectId: String
    var projectPathSnapshot: String?
}

struct ConversationExecutionTarget: Hashable, Decodable, Sendable {
    var kind: String
    var projectId: String?
    var projectRoot: String?
    var worktreePath: String?
}

struct ConversationOpenOutcome: Decodable, Sendable {
    var conversation: HostConversation?
    var workspace: SessionWorkspaceLoadOutcome?
}

struct SessionWorkspaceLoadOutcome: Decodable, Sendable {
    var status: String
    var workspace: SessionWorkspacePayload?
}

struct SessionWorkspacePayload: Decodable, Sendable {
    var resources: [SessionWorkspaceResource]?
}

struct SessionWorkspaceResource: Decodable, Sendable {
    var kind: String
    var terminalId: String?
}
