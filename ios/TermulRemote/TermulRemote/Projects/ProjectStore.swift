import Foundation
import Observation

@MainActor
@Observable
final class ProjectStore {
    var projects: [HostProject] = []
    var active: HostProject?
    var errorMessage: String?

    private var http: HostHTTP?
    private weak var socket: AcpSocket?

    func attach(http: HostHTTP, socket: AcpSocket) {
        self.http = http
        self.socket = socket
    }

    func refresh() async {
        guard let http else { return }
        do {
            let payload: ProjectListPayload = try await http.get("projects")
            projects = payload.projects
            if active == nil {
                active = payload.projects.first(where: { $0.id == payload.defaultProjectId })
                    ?? payload.projects.first(where: { !$0.isArchived })
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func select(_ project: HostProject) async {
        active = project
        guard let socket else { return }
        do {
            let reply = try await socket.request(
                "switch_project",
                payload: ["projectId": project.id],
                as: SwitchProjectReply.self
            )
            if var current = active, let cwd = reply.cwd, !cwd.isEmpty {
                current.path = cwd
                active = current
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
