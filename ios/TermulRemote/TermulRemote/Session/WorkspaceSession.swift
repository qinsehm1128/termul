import Foundation
import Observation
import UIKit

enum SessionPhase: Equatable {
    case idle
    case connecting
    case connected
    case failed(String)
}

@MainActor
@Observable
final class WorkspaceSession {
    let origin: URL
    let http: HostHTTP
    let acp = AcpSocket()
    let terminalSocket = TerminalSocket()
    let chat = ChatStore()
    let projects = ProjectStore()
    let files = FileStore()
    let terminals = TerminalStore()

    var phase: SessionPhase = .idle

    init(origin: URL) {
        self.origin = origin
        http = HostHTTP(origin: origin)
        chat.attach(socket: acp)
        projects.attach(http: http, socket: acp)
        files.attach(http: http)
        terminals.attach(socket: terminalSocket, origin: origin)
    }

    func start() async {
        phase = .connecting
        do {
            try await http.probeHealth()
            try await acp.connect(origin: origin)
            await projects.refresh()
            do {
                try await terminals.ensureConnected()
                if let project = projects.active {
                    await terminals.refresh(projectId: project.id)
                }
            } catch {
                terminals.errorMessage = error.localizedDescription
            }
            await chat.refreshSessions()
            if let first = chat.sessions.first {
                await chat.open(first)
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

    func selectProject(_ project: HostProject) async {
        await projects.select(project)
        await terminals.refresh(projectId: project.id)
        if let path = project.path {
            await files.openRoot(path)
        }
    }
}
