import Foundation
import Observation

struct LiveTerminal: Identifiable, Hashable, Sendable {
    let id: String
    var claim: String?
    var lastSeq: Int
    var cols: Int
    var rows: Int
    var title: String
    var cwd: String?
    var gitBranch: String?
    /// True only for PTYs this phone spawned. Desktop-owned tabs are watch-only
    /// for geometry so a phone resize does not reflow the desktop.
    var owned: Bool
}

@MainActor
@Observable
final class TerminalStore {
    var terminals: [LiveTerminal] = []
    var activeId: String?
    var pendingOutput: [String: Data] = [:]
    var errorMessage: String?
    var isConnecting = false
    var onFeed: (@MainActor (String, Data) -> Void)?

    private var socket: TerminalSocket?
    private var origin: URL?
    private var credentials: HostCredentials?
    private var watchedId: String?
    private var lastConversationId: String?
    private var lastProjectId: String?

    func attach(socket: TerminalSocket, origin: URL, credentials: HostCredentials) {
        self.socket = socket
        self.origin = origin
        self.credentials = credentials
        socket.onBytes = { [weak self] terminalId, data in
            guard let self else { return }
            if let existing = self.terminals.firstIndex(where: { $0.id == terminalId }) {
                self.terminals[existing].lastSeq += 1
            }
            if let onFeed {
                onFeed(terminalId, data)
            } else {
                var buffer = self.pendingOutput[terminalId] ?? Data()
                buffer.append(data)
                self.pendingOutput[terminalId] = buffer
            }
        }
        socket.onExit = { [weak self] terminalId in
            guard let self else { return }
            self.terminals.removeAll { $0.id == terminalId }
            self.pendingOutput.removeValue(forKey: terminalId)
            if self.watchedId == terminalId {
                self.watchedId = nil
            }
            if self.activeId == terminalId {
                self.activeId = self.terminals.first?.id
                if let next = self.activeId {
                    Task { await self.open(next) }
                }
            }
        }
    }

    func ensureConnected() async throws {
        guard let socket, let origin, let credentials else {
            throw HostError.unexpected(String(localized: "Not connected."))
        }
        if socket.isConnected { return }
        try await socket.connect(origin: origin, credentials: credentials)
    }

    func refresh(conversationId: String?, projectId: String?) async {
        lastConversationId = conversationId
        lastProjectId = projectId
        isConnecting = true
        defer { isConnecting = false }
        do {
            try await ensureConnected()
            guard let socket else { return }
            let listed = try await socket.list(conversationId: conversationId, projectId: projectId)
            let ownedById = Dictionary(uniqueKeysWithValues: terminals.filter(\.owned).map { ($0.id, $0) })
            let previousActive = activeId
            terminals = listed.map { item in
                let owned = ownedById[item.id]
                return LiveTerminal(
                    id: item.id,
                    claim: owned?.claim,
                    lastSeq: owned?.lastSeq ?? 0,
                    cols: item.cols ?? owned?.cols ?? 80,
                    rows: item.rows ?? owned?.rows ?? 24,
                    title: Self.title(for: item),
                    cwd: item.cwd,
                    gitBranch: item.gitBranch,
                    owned: owned != nil
                )
            }
            if let previousActive, terminals.contains(where: { $0.id == previousActive }) {
                activeId = previousActive
            } else {
                activeId = terminals.first?.id
            }
            if let activeId, watchedId != activeId {
                await open(activeId)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func open(_ terminalId: String) async {
        guard let socket else { return }
        do {
            try await ensureConnected()
            if let watchedId, watchedId != terminalId {
                await socket.detach(terminalId: watchedId)
            }
            _ = try await socket.watch(terminalId: terminalId, lastSeq: 0)
            watchedId = terminalId
            activeId = terminalId
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func spawn(conversationId: String, projectId: String?, cols: Int = 80, rows: Int = 24) async {
        isConnecting = true
        defer { isConnecting = false }
        do {
            try await ensureConnected()
            guard let socket else { return }
            let spawned = try await socket.spawn(
                conversationId: conversationId,
                projectId: projectId,
                cols: cols,
                rows: rows
            )
            let live = LiveTerminal(
                id: spawned.id,
                claim: spawned.claim,
                lastSeq: 0,
                cols: spawned.cols ?? cols,
                rows: spawned.rows ?? rows,
                title: String(localized: "Terminal"),
                cwd: spawned.cwd,
                gitBranch: nil,
                owned: true
            )
            if !terminals.contains(where: { $0.id == live.id }) {
                terminals.append(live)
            }
            await open(live.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func write(_ data: String) async {
        guard let socket, let activeId else { return }
        do {
            try await socket.write(terminalId: activeId, data: data)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func resize(cols: Int, rows: Int) async {
        guard let socket, let activeId else { return }
        guard let index = terminals.firstIndex(where: { $0.id == activeId }) else { return }
        terminals[index].cols = cols
        terminals[index].rows = rows
        guard terminals[index].owned else { return }
        _ = try? await socket.resize(terminalId: activeId, cols: cols, rows: rows)
    }

    func consumeOutput(for terminalId: String) -> Data? {
        let data = pendingOutput.removeValue(forKey: terminalId)
        return data?.isEmpty == false ? data : nil
    }

    private static func title(for item: LiveTerminalSummary) -> String {
        if let title = item.title, !title.isEmpty {
            if let branch = item.gitBranch, !branch.isEmpty {
                return "\(title) · \(branch)"
            }
            return title
        }
        if let cwd = item.cwd, let name = cwd.split(separator: "/").last, !name.isEmpty {
            return String(name)
        }
        return String(localized: "Terminal")
    }
}
