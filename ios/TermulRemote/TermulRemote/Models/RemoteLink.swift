import Foundation

enum WorkspaceSurface: String, Hashable, CaseIterable, Identifiable {
    case chat
    case terminal

    var id: String { rawValue }
}

struct RemoteLink: Identifiable, Hashable, Codable {
    let id: UUID
    var title: String
    var accessURL: URL
    var createdAt: Date

    init(id: UUID = UUID(), title: String? = nil, accessURL: URL, createdAt: Date = .now) {
        self.id = id
        self.title = title ?? RemoteLink.displayTitle(for: accessURL)
        self.accessURL = accessURL
        self.createdAt = createdAt
    }

    var originHost: String {
        accessURL.host() ?? accessURL.absoluteString
    }

    var originURL: URL {
        var components = URLComponents(url: accessURL, resolvingAgainstBaseURL: false) ?? URLComponents()
        components.fragment = nil
        components.query = nil
        components.path = ""
        return components.url ?? accessURL
    }

    func url(for surface: WorkspaceSurface) -> URL {
        var base = accessURL.absoluteString
        if let hash = base.firstIndex(of: "#") {
            base = String(base[..<hash])
        }
        while base.hasSuffix("/") {
            base.removeLast()
        }
        switch surface {
        case .chat:
            return accessURL
        case .terminal:
            return URL(string: "\(base)/#/terminal") ?? accessURL
        }
    }

    static func parse(_ raw: String) throws -> RemoteLink {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed), let scheme = url.scheme?.lowercased() else {
            throw RemoteLinkError.invalidURL
        }
        if scheme == "termul" {
            return try parseDeepLink(url)
        }
        guard scheme == "https" else {
            throw RemoteLinkError.httpsRequired
        }
        guard url.host() != nil else {
            throw RemoteLinkError.invalidURL
        }
        return RemoteLink(accessURL: url)
    }

    private static func parseDeepLink(_ url: URL) throws -> RemoteLink {
        let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        let embedded = components?.queryItems?.first(where: { $0.name == "url" })?.value
        guard let embedded, let nested = URL(string: embedded) else {
            throw RemoteLinkError.invalidURL
        }
        return try parse(nested.absoluteString)
    }

    private static func displayTitle(for url: URL) -> String {
        url.host() ?? String(localized: "Saved connection")
    }
}

enum RemoteLinkError: LocalizedError {
    case invalidURL
    case httpsRequired

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            String(localized: "That does not look like a Termul access link.")
        case .httpsRequired:
            String(localized: "iOS only opens HTTPS tunnels. Use a TLS reverse proxy in front of FRP, or Cloudflare.")
        }
    }
}
