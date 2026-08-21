import Foundation
import Observation

@MainActor
@Observable
final class ConnectionStore {
    private static let storageKey = "termul.remote.savedLinks"

    var savedLinks: [RemoteLink] = []
    var path: [Route] = []
    var errorMessage: String?

    init() {
        savedLinks = Self.load()
    }

    func connect(to raw: String) {
        do {
            let link = try RemoteLink.parse(raw)
            remember(link)
            errorMessage = nil
            path.append(.session(link))
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func connect(link: RemoteLink) {
        remember(link)
        errorMessage = nil
        path.append(.session(link))
    }

    func forget(_ link: RemoteLink) {
        savedLinks.removeAll { $0.id == link.id }
        persist()
    }

    func openIncomingURL(_ url: URL) {
        connect(to: url.absoluteString)
    }

    func dismissError() {
        errorMessage = nil
    }

    enum Route: Hashable {
        case session(RemoteLink)
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
