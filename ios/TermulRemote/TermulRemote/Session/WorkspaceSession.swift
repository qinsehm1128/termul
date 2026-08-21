import Foundation
import Observation
import UIKit

enum SessionPhase: Equatable {
    case idle
    case connecting
    case connected
    case failed(String)
}

enum WorkspaceKind: Equatable {
    case home
    case conversation
    case project
}

@MainActor
@Observable
final class WorkspaceSession {
    let origin: URL
    let credentials: HostCredentials
    let http: HostHTTP
    let acp = AcpSocket()
    let terminalSocket = TerminalSocket()
    let chat = ChatStore()
    let conversations = ConversationStore()
    let projects = ProjectStore()
    let files = FileStore()
    let terminals = TerminalStore()

    /// Chat sits on a Conversation. A project workspace is terminals only.
    var showChat = true
    var workspace: WorkspaceKind = .home
    var phase: SessionPhase = .idle

    init(accessURL: URL) {
        credentials = HostCredentials(accessURL: accessURL)
        origin = credentials.origin
        http = HostHTTP(origin: origin, credentials: credentials)
        chat.attach(socket: acp)
        conversations.attach(http: http)
        projects.attach(http: http, socket: acp)
        files.attach(http: http)
        terminals.attach(socket: terminalSocket, origin: origin, credentials: credentials)
    }

    func start() async {
        phase = .connecting
        do {
            try await http.probeHealth()
            try await acp.connect(origin: origin, credentials: credentials)
            await conversations.refresh()
            await projects.refresh()
            do {
                try await terminals.ensureConnected()
            } catch {
                terminals.errorMessage = error.localizedDescription
            }
            phase = .connected
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    func retry() async {
        stop()
        await start()
    }

    func stop() {
        acp.stop()
        terminalSocket.stop()
        phase = .idle
    }

    func handleScene(isBackground: Bool) {
        acp.handleLifecycle(isBackground: isBackground)
    }

    func selectConversation(_ conversation: HostConversation) async {
        projects.clearSelection()
        workspace = .conversation
        showChat = true
        _ = await conversations.open(conversation)
        await refreshActiveTerminals()
        await files.openRoot(conversation.workspaceCwd)
    }

    func selectProject(_ project: HostProject) async {
        conversations.clearSelection()
        workspace = .project
        showChat = false
        await projects.select(project)
        await refreshActiveTerminals()
        if let path = project.path {
            await files.openRoot(path)
        }
    }

    func leaveWorkspace() {
        workspace = .home
        conversations.clearSelection()
        projects.clearSelection()
        terminals.terminals = []
        terminals.activeId = nil
        chat.messages = []
        chat.activeSessionId = nil
        showChat = true
    }

    func leaveConversation() {
        leaveWorkspace()
    }

    func revealTerminal(_ terminalId: String) async {
        showChat = false
        await terminals.open(terminalId)
    }

    func revealChat() {
        showChat = true
    }

    func refreshActiveTerminals() async {
        switch workspace {
        case .conversation:
            guard let conversation = conversations.active else {
                terminals.terminals = []
                terminals.activeId = nil
                return
            }
            await terminals.refresh(conversationId: conversation.id, projectId: nil)
        case .project:
            guard let project = projects.active else {
                terminals.terminals = []
                terminals.activeId = nil
                return
            }
            await terminals.refresh(conversationId: nil, projectId: project.id)
        case .home:
            terminals.terminals = []
            terminals.activeId = nil
        }
    }
}
