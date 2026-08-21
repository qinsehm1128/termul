import Foundation
import Observation

@MainActor
@Observable
final class ConnectionStore {
    private static let storageKey = "termul.remote.savedLinks"

    var savedLinks: [RemoteLink] = []
    var activeLink: RemoteLink?
    var surface: WorkspaceSurface = .chat
    var errorMessage: String?
    var isScanning = false

    init() {
        savedLinks = Self.load()
    }

    func connect(to raw: String) {
        do {
            let link = try RemoteLink.parse(raw)
            remember(link)
            errorMessage = nil
            surface = .chat
            activeLink = link
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func connect(link: RemoteLink, surface: WorkspaceSurface = .chat) {
        remember(link)
        errorMessage = nil
        self.surface = surface
        activeLink = link
    }

    func disconnect() {
        activeLink = nil
        surface = .chat
    }

    func forget(_ link: RemoteLink) {
        savedLinks.removeAll { $0.id == link.id }
        if activeLink?.id == link.id {
            disconnect()
        }
        persist()
    }

    func openIncomingURL(_ url: URL) {
        connect(to: url.absoluteString)
    }

    func dismissError() {
        errorMessage = nil
    }

    private func remember(_ link: RemoteLink) {
        savedLinks.removeAll { $0.accessURL == link.accessURL }
        savedLinks.insert(link, at: 0)
        persist()
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(savedLinks) else { return }
        UserDefaults.standard.set(data, forKey: Self.storageKey)
    }

    private static func load() -> [RemoteLink] {
        guard let data = UserDefaults.standard.data(forKey: storageKey) else { return [] }
        return (try? JSONDecoder().decode([RemoteLink].self, from: data)) ?? []
    }
}
