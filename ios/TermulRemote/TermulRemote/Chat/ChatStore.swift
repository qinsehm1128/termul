import Foundation
import Observation

enum ChatRole: String, Sendable {
    case user
    case agent
    case thought
}

struct ChatMessage: Identifiable, Hashable, Sendable {
    let id: String
    var role: ChatRole
    var text: String
    var streaming: Bool
}

struct ToolCard: Identifiable, Hashable, Sendable {
    let id: String
    var title: String
    var status: String
}

struct PermissionCard: Identifiable, Hashable, Sendable {
    let id: String
    var agentId: String
    var title: String
    var options: [PermissionChoice]
}

struct PermissionChoice: Identifiable, Hashable, Sendable {
    var id: String
    var name: String
}

struct QuestionCard: Identifiable, Hashable, Sendable {
    let id: String
    var agentId: String
    var question: String
    var options: [QuestionChoice]
}

struct QuestionChoice: Identifiable, Hashable, Sendable {
    var id: String
    var label: String
}

@MainActor
@Observable
final class ChatStore {
    var sessions: [PersistedSession] = []
    var activeSessionId: String?
    var activeAgentId: String?
    var activeCwd: String = ""
    var messages: [ChatMessage] = []
    var tools: [ToolCard] = []
    var permissions: [PermissionCard] = []
    var questions: [QuestionCard] = []
    var isSending = false
    var isLoading = false
    var errorMessage: String?
    var lastSeq: [String: UInt64] = [:]

    private weak var socket: AcpSocket?

    func attach(socket: AcpSocket) {
        self.socket = socket
        socket.onEvent = { [weak self] type, sid, seq, payload in
            self?.handle(type: type, sid: sid, seq: seq, payload: payload)
        }
    }

    func refreshSessions() async {
        guard let socket else { return }
        do {
            sessions = try await socket.request("list_persisted_sessions", as: [PersistedSession].self)
        } catch let error as HostError {
            if case .ipc(_, let code) = error, code == "unsupported" {
                sessions = []
                return
            }
            errorMessage = error.localizedDescription
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func open(_ session: PersistedSession) async {
        isLoading = true
        defer { isLoading = false }
        activeSessionId = session.sessionId
        activeAgentId = session.runtimeAgentId
        activeCwd = session.cwd ?? ""
        messages = []
        tools = []
        permissions = []
        questions = []
        do {
            if let payload = try? await socket?.request(
                "get_session_payload",
                payload: ["sessionId": session.sessionId],
                as: SessionPayload.self
            ) {
                activeAgentId = payload.metadata?.agentId ?? session.runtimeAgentId
                activeCwd = payload.metadata?.cwd ?? session.cwd ?? ""
                messages = payload.messages.compactMap(Self.message(from:))
                if let watermark = payload.metadata?.lastSeq {
                    lastSeq[session.sessionId] = watermark
                }
            }
            if let agentId = activeAgentId, !activeCwd.isEmpty {
                _ = try? await socket?.request(
                    "resume_session",
                    payload: [
                        "agentId": agentId,
                        "sessionId": session.sessionId,
                        "cwd": activeCwd
                    ]
                )
            }
            try await subscribe(sessionId: session.sessionId, lastSeq: lastSeq[session.sessionId])
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func startInConversation(_ conversation: HostConversation) async {
        await startNewChat(cwd: conversation.workspaceCwd, conversationId: conversation.id)
    }

    func startNewChat(cwd: String, conversationId: String?, projectId: String? = nil) async {
        guard let socket else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            let agentId = try await ensureAgent()
            var payload: [String: Any] = [
                "agentId": agentId,
                "cwd": cwd,
                "ephemeral": false
            ]
            if let conversationId, !conversationId.isEmpty {
                payload["conversationId"] = conversationId
            }
            if let projectId {
                payload["projectId"] = projectId
            }
            let created = try await socket.request("create_session", payload: payload, as: NewSessionOutcome.self)
            activeAgentId = agentId
            activeSessionId = created.sessionId
            activeCwd = cwd
            messages = []
            tools = []
            permissions = []
            questions = []
            lastSeq[created.sessionId] = 0
            try await subscribe(sessionId: created.sessionId, lastSeq: nil)
            await refreshSessions()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func send(_ text: String, in conversation: HostConversation? = nil) async {
        if activeSessionId == nil, let conversation {
            await startInConversation(conversation)
        }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let socket, let sessionId = activeSessionId, let agentId = activeAgentId else {
            if activeAgentId == nil {
                errorMessage = String(localized: "No agent is running on the host. Start one on the desktop, then try again.")
            }
            return
        }
        isSending = true
        let turnId = UUID().uuidString
        messages.append(
            ChatMessage(id: "turn:\(turnId)", role: .user, text: trimmed, streaming: false)
        )
        do {
            _ = try await socket.request(
                "send_prompt",
                payload: [
                    "agentId": agentId,
                    "sessionId": sessionId,
                    "text": trimmed,
                    "turnId": turnId
                ]
            )
        } catch {
            errorMessage = error.localizedDescription
        }
        isSending = false
    }

    func cancel() async {
        guard let socket, let sessionId = activeSessionId, let agentId = activeAgentId else { return }
        _ = try? await socket.request(
            "cancel_prompt",
            payload: ["agentId": agentId, "sessionId": sessionId]
        )
        isSending = false
    }

    func respond(permission: PermissionCard, optionId: String?) async {
        permissions.removeAll { $0.id == permission.id }
        guard let socket else { return }
        var payload: [String: Any] = [
            "agentId": permission.agentId,
            "requestId": permission.id
        ]
        if let optionId {
            payload["optionId"] = optionId
        }
        _ = try? await socket.request("respond_permission", payload: payload)
    }

    func answer(question: QuestionCard, values: [String]) async {
        questions.removeAll { $0.id == question.id }
        guard let socket else { return }
        _ = try? await socket.request(
            "answer_question",
            payload: [
                "agentId": question.agentId,
                "questionId": question.id,
                "values": values
            ]
        )
    }

    private func subscribe(sessionId: String, lastSeq: UInt64?) async throws {
        var payload: [String: Any] = ["sessionId": sessionId]
        if let lastSeq {
            payload["lastSeq"] = lastSeq
        }
        do {
            _ = try await socket?.request("subscribe", payload: payload)
        } catch let error as HostError {
            if case .ipc(_, let code) = error, code == "stale" {
                _ = try await socket?.request("subscribe", payload: ["sessionId": sessionId])
                return
            }
            throw error
        }
    }

    private func ensureAgent() async throws -> String {
        guard let socket else {
            throw HostError.unexpected(String(localized: "Not connected."))
        }
        if let existing = try? await socket.request("list_agents", as: [String].self), let first = existing.first {
            return first
        }
        let catalog = try await socket.request("list_acp_catalog", as: AcpCatalog.self)
        guard let agent = catalog.agents.first(where: { $0.status == "ready" && $0.installed != nil }),
              let installed = agent.installed
        else {
            throw HostError.unexpected(String(localized: "No agent is running on the host. Start one on the desktop, then try again."))
        }
        let spawned = try await socket.request(
            "spawn_agent",
            payload: [
                "config": [
                    "configId": agent.id,
                    "name": agent.name,
                    "command": installed.command,
                    "args": installed.args ?? [],
                    "env": [:] as [String: String]
                ]
            ],
            as: SpawnAgentResult.self
        )
        return spawned.agentId
    }

    private func handle(type: String, sid: String?, seq: UInt64, payload: Data) {
        if let sid, seq > 0 {
            lastSeq[sid] = max(lastSeq[sid] ?? 0, seq)
        }
        switch type {
        case "user_prompt":
            if let event = try? JSONDecoder().decode(UserPromptEvent.self, from: payload) {
                let id = event.turnId.map { "turn:\($0)" } ?? "user:\(seq)"
                let text = event.content.compactMap(\.text).joined()
                if let index = messages.firstIndex(where: { $0.id == id }) {
                    messages[index].text = text
                    messages[index].streaming = false
                } else {
                    messages.append(ChatMessage(id: id, role: .user, text: text, streaming: false))
                }
            }
        case "message_chunk":
            if let event = try? JSONDecoder().decode(MessageChunkEvent.self, from: payload) {
                appendChunk(event)
            }
        case "tool_call":
            if let event = try? JSONDecoder().decode(ToolCallEvent.self, from: payload) {
                upsertTool(event.toolCall)
            }
        case "tool_call_update":
            if let event = try? JSONDecoder().decode(ToolCallUpdateEvent.self, from: payload) {
                upsertTool(event.update)
            }
        case "permission_request":
            if let event = try? JSONDecoder().decode(PermissionEvent.self, from: payload) {
                permissions.append(
                    PermissionCard(
                        id: event.requestId,
                        agentId: event.agentId,
                        title: event.toolCall.title ?? String(localized: "Permission required"),
                        options: event.options.map { PermissionChoice(id: $0.optionId, name: $0.name) }
                    )
                )
            }
        case "question_request":
            if let event = try? JSONDecoder().decode(QuestionEvent.self, from: payload) {
                questions.append(
                    QuestionCard(
                        id: event.questionId,
                        agentId: event.agentId,
                        question: event.question,
                        options: event.options.map { QuestionChoice(id: $0.value, label: $0.label) }
                    )
                )
            }
        case "prompt_complete":
            if let last = messages.indices.last {
                messages[last].streaming = false
            }
            isSending = false
        case "session_info_update":
            if let event = try? JSONDecoder().decode(SessionInfoEvent.self, from: payload),
               let title = event.title,
               let sid,
               let index = sessions.firstIndex(where: { $0.sessionId == sid }) {
                sessions[index].title = title
            }
        case "chat_history_changed":
            Task { await refreshSessions() }
        default:
            break
        }
    }

    private func appendChunk(_ event: MessageChunkEvent) {
        let role = ChatRole(rawValue: event.role) ?? .agent
        let text = event.content.text ?? ""
        if let last = messages.last, last.role == role, last.streaming {
            messages[messages.count - 1].text += text
            return
        }
        messages.append(
            ChatMessage(
                id: "msg-\(UUID().uuidString)",
                role: role,
                text: text,
                streaming: true
            )
        )
    }

    private func upsertTool(_ update: ToolCallPatch) {
        if let index = tools.firstIndex(where: { $0.id == update.toolCallId }) {
            if let title = update.title { tools[index].title = title }
            if let status = update.status { tools[index].status = status }
        } else {
            tools.append(
                ToolCard(
                    id: update.toolCallId,
                    title: update.title ?? update.toolCallId,
                    status: update.status ?? "pending"
                )
            )
        }
    }

    private static func message(from wire: WireChatMessage) -> ChatMessage? {
        let text = (wire.blocks ?? []).compactMap(\.text).joined()
        guard !text.isEmpty || wire.role != nil else { return nil }
        return ChatMessage(
            id: wire.id ?? UUID().uuidString,
            role: ChatRole(rawValue: wire.role ?? "agent") ?? .agent,
            text: text,
            streaming: wire.streaming ?? false
        )
    }
}

private struct UserPromptEvent: Decodable {
    var turnId: String?
    var content: [ContentBlock]
}

private struct MessageChunkEvent: Decodable {
    var role: String
    var content: ContentBlock
}

private struct ToolCallEvent: Decodable {
    var toolCall: ToolCallPatch
}

private struct ToolCallUpdateEvent: Decodable {
    var update: ToolCallPatch
}

private struct ToolCallPatch: Decodable {
    var toolCallId: String
    var title: String?
    var status: String?
}

private struct PermissionEvent: Decodable {
    var agentId: String
    var requestId: String
    var toolCall: ToolCallPatch
    var options: [PermissionOptionWire]
}

private struct PermissionOptionWire: Decodable {
    var optionId: String
    var name: String
}

private struct QuestionEvent: Decodable {
    var agentId: String
    var questionId: String
    var question: String
    var options: [QuestionOptionWire]
}

private struct QuestionOptionWire: Decodable {
    var value: String
    var label: String
}

private struct SessionInfoEvent: Decodable {
    var title: String?
}
