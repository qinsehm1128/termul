import Foundation

struct SpawnedTerminal: Decodable, Sendable {
    let id: String
    var shell: String?
    var cwd: String?
    var pid: Int?
    var cols: Int?
    var rows: Int?
    var claim: String?
}

struct TerminalAttachResult: Decodable, Sendable {
    let id: String
    var shell: String?
    var cwd: String?
    var pid: Int?
    var cols: Int?
    var rows: Int?
    var latestSeq: Int?
    var gap: Bool?
}

struct LiveTerminalList: Decodable, Sendable {
    var terminals: [LiveTerminalSummary]
}

struct LiveTerminalSummary: Decodable, Identifiable, Sendable {
    let id: String
    var shell: String?
    var cwd: String?
    var pid: Int?
    var cols: Int?
    var rows: Int?
    var conversationId: String?
    var projectId: String?
    var title: String?
    var gitBranch: String?
}

@MainActor
@Observable
final class TerminalSocket {
    private(set) var isConnected = false

    var onBytes: (@MainActor (String, Data) -> Void)?
    var onExit: (@MainActor (String) -> Void)?

    private var session: URLSession?
    private var task: URLSessionWebSocketTask?
    private var receiveTask: Task<Void, Never>?
    private var pending: [String: CheckedContinuation<Data, Error>] = [:]
    private var requestSerial = 0

    func connect(origin: URL, credentials: HostCredentials) async throws {
        stop()
        guard credentials.bearer?.isEmpty == false else {
            throw HostError.unexpected(String(localized: "This access link is missing its token. Scan the QR again."))
        }
        var components = URLComponents(url: origin, resolvingAgainstBaseURL: false) ?? URLComponents()
        components.scheme = origin.scheme?.lowercased() == "https" ? "wss" : "ws"
        components.path = "/terminal/ws"
        components.fragment = nil
        components.query = nil
        guard let wsURL = components.url else {
            throw HostError.unexpected("Invalid terminal URL")
        }
        var request = URLRequest(url: wsURL, timeoutInterval: 20)
        request.assumesHTTP3Capable = false
        credentials.apply(to: &request)
        let session = URLSession(configuration: .default)
        self.session = session
        let task = session.webSocketTask(with: request)
        self.task = task
        task.resume()
        receiveTask = Task { await self.receiveLoop() }
        isConnected = true
    }

    func spawn(conversationId: String, projectId: String?, cols: Int, rows: Int) async throws -> SpawnedTerminal {
        var payload: [String: Any] = [
            "conversationId": conversationId,
            "cwdSource": "workspace",
            "cols": cols,
            "rows": rows
        ]
        if let projectId, !projectId.isEmpty {
            payload["projectId"] = projectId
        }
        return try await request("spawn", payload: payload, as: SpawnedTerminal.self)
    }

    func attach(terminalId: String, claim: String, lastSeq: Int) async throws -> TerminalAttachResult {
        try await request(
            "attach",
            payload: [
                "terminalId": terminalId,
                "claim": claim,
                "lastSeq": lastSeq
            ],
            as: TerminalAttachResult.self
        )
    }

    func list(conversationId: String?, projectId: String?) async throws -> [LiveTerminalSummary] {
        var payload: [String: Any] = [:]
        if let conversationId, !conversationId.isEmpty {
            payload["conversationId"] = conversationId
        }
        if let projectId, !projectId.isEmpty {
            payload["projectId"] = projectId
        }
        let reply: LiveTerminalList = try await request(
            "list",
            payload: payload,
            as: LiveTerminalList.self
        )
        return reply.terminals
    }

    func watch(terminalId: String, lastSeq: Int) async throws -> TerminalAttachResult {
        try await request(
            "watch",
            payload: [
                "terminalId": terminalId,
                "lastSeq": lastSeq
            ],
            as: TerminalAttachResult.self
        )
    }

    func detach(terminalId: String) async {
        _ = try? await request(
            "detach",
            payload: ["terminalId": terminalId],
            as: EmptyPayload.self
        )
    }

    func write(terminalId: String, data: String) async throws {
        _ = try await request(
            "write",
            payload: ["terminalId": terminalId, "data": data],
            as: EmptyPayload.self
        )
    }

    func resize(terminalId: String, cols: Int, rows: Int) async throws {
        _ = try await request(
            "resize",
            payload: ["terminalId": terminalId, "cols": cols, "rows": rows],
            as: EmptyPayload.self
        )
    }

    func stop() {
        receiveTask?.cancel()
        receiveTask = nil
        for (_, continuation) in pending {
            continuation.resume(throwing: HostError.network(String(localized: "Disconnected from the host.")))
        }
        pending.removeAll()
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        session?.invalidateAndCancel()
        session = nil
        isConnected = false
    }

    private func request<T: Decodable>(_ type: String, payload: [String: Any], as _: T.Type) async throws -> T {
        requestSerial += 1
        let id = "term-\(requestSerial)"
        let body: [String: Any] = ["id": id, "type": type, "payload": payload]
        let data = try WireJSON.data(from: body)
        let reply = try await withCheckedThrowingContinuation { continuation in
            pending[id] = continuation
            task?.send(.string(String(data: data, encoding: .utf8) ?? "")) { [weak self] error in
                Task { @MainActor in
                    if let error {
                        self?.pending.removeValue(forKey: id)?.resume(throwing: HostError.network(error.localizedDescription))
                    }
                }
            }
        }
        if T.self == EmptyPayload.self {
            return EmptyPayload() as! T
        }
        return try JSONDecoder().decode(T.self, from: reply)
    }

    private func receiveLoop() async {
        while let task, !Task.isCancelled {
            do {
                let message = try await task.receive()
                let data: Data
                switch message {
                case .data(let value):
                    data = value
                case .string(let value):
                    data = Data(value.utf8)
                @unknown default:
                    continue
                }
                handleFrame(data)
            } catch {
                isConnected = false
                return
            }
        }
    }

    private func handleFrame(_ data: Data) {
        guard let object = try? WireJSON.object(from: data) else { return }
        if let id = object["id"] as? String, object["success"] is Bool {
            handleReply(id: id, object: object)
            return
        }
        guard let type = object["type"] as? String else { return }
        switch type {
        case "data":
            if let terminalId = object["terminalId"] as? String {
                onBytes?(terminalId, Self.bytes(from: object["data"]))
            }
        case "replay":
            if let terminalId = object["terminalId"] as? String,
               let chunks = object["chunks"] as? [[String: Any]] {
                for chunk in chunks {
                    onBytes?(terminalId, Self.bytes(from: chunk["data"]))
                }
            }
        case "event":
            if let payload = object["payload"] as? [String: Any],
               payload["type"] as? String == "exit",
               let terminalId = payload["terminal_id"] as? String {
                onExit?(terminalId)
            }
        default:
            break
        }
    }

    private func handleReply(id: String, object: [String: Any]) {
        guard let continuation = pending.removeValue(forKey: id) else { return }
        let success = object["success"] as? Bool ?? false
        if success {
            if let payload = object["data"], JSONSerialization.isValidJSONObject(payload),
               let encoded = try? JSONSerialization.data(withJSONObject: payload) {
                continuation.resume(returning: encoded)
            } else {
                continuation.resume(returning: Data("{}".utf8))
            }
        } else {
            let message = (object["error"] as? String) ?? String(localized: "The host rejected the request.")
            let code = (object["code"] as? String) ?? "NETWORK_ERROR"
            continuation.resume(throwing: HostError.ipc(message, code))
        }
    }

    private static func bytes(from raw: Any?) -> Data {
        if let numbers = raw as? [Any] {
            let bytes = numbers.compactMap { item -> UInt8? in
                if let number = item as? NSNumber { return UInt8(truncating: number) }
                if let number = item as? Int { return UInt8(clamping: number) }
                return nil
            }
            return Data(bytes)
        }
        if let text = raw as? String {
            return Data(text.utf8)
        }
        return Data()
    }
}
