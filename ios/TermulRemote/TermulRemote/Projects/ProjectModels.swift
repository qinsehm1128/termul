import Foundation

struct HostProject: Identifiable, Hashable, Decodable, Sendable {
    let id: String
    var name: String
    var color: String?
    var path: String?
    var isArchived: Bool
    var isDefault: Bool
}

struct ProjectListPayload: Decodable, Sendable {
    var projects: [HostProject]
    var defaultProjectId: String?
}

struct CatalogAgent: Decodable, Sendable, Identifiable {
    let id: String
    var name: String
    var status: String?
    var installed: InstalledBinary?
}

struct InstalledBinary: Decodable, Sendable {
    var command: String
    var args: [String]?
}

struct AcpCatalog: Decodable, Sendable {
    var agents: [CatalogAgent]
}

struct DirectoryEntry: Identifiable, Hashable, Decodable, Sendable {
    var name: String
    var path: String
    var type: String
    var fileExtension: String?
    var size: Int?
    var modifiedAt: Double?

    var id: String { path }
    var isDirectory: Bool { type == "directory" }

    enum CodingKeys: String, CodingKey {
        case name, path, type, size, modifiedAt
        case fileExtension = "extension"
    }
}

struct FileContent: Decodable, Sendable {
    var content: String
    var encoding: String?
    var size: Int?
    var modifiedAt: Double?
}

struct PersistedSession: Identifiable, Decodable, Sendable {
    var storageKey: String?
    var sessionId: String
    var runtimeAgentId: String?
    var projectId: String?
    var cwd: String?
    var title: String?
    var createdAt: Double?
    var lastActivityAt: Double?
    var status: String?
    var messageCount: Int?
    var lastSeq: UInt64?

    var id: String { sessionId }

    var displayTitle: String {
        if let title, !title.isEmpty { return title }
        return sessionId
    }
}

struct SessionPayload: Decodable, Sendable {
    var metadata: SessionMetadata?
    var messages: [WireChatMessage]
}

struct SessionMetadata: Decodable, Sendable {
    var id: String?
    var agentId: String?
    var title: String?
    var cwd: String?
    var projectId: String?
    var lastSeq: UInt64?
    var status: String?
}

struct WireChatMessage: Decodable, Sendable {
    var id: String?
    var role: String?
    var blocks: [ContentBlock]?
    var streaming: Bool?
    var timestamp: Double?
    var seq: UInt64?
}

struct ContentBlock: Codable, Hashable, Sendable {
    var type: String
    var text: String?
}

struct NewSessionOutcome: Decodable, Sendable {
    var sessionId: String
}

struct SpawnAgentResult: Decodable, Sendable {
    var agentId: String
}

struct SwitchProjectReply: Decodable, Sendable {
    var status: String?
    var projectId: String?
    var sessionId: String?
    var cwd: String?
}
