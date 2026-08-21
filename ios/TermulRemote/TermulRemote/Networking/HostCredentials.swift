import Foundation

struct HostCredentials: Sendable {
    let origin: URL
    let bearer: String?

    init(accessURL: URL) {
        origin = HostCredentials.origin(from: accessURL)
        bearer = HostCredentials.accessToken(from: accessURL)
    }

    func apply(to request: inout URLRequest) {
        if let bearer, !bearer.isEmpty {
            request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        }
        // Desktop `/ws` and `/terminal/ws` reject upgrades without an allowed
        // browser Origin. URLSession does not send one unless we set it.
        request.setValue(originString, forHTTPHeaderField: "Origin")
    }

    var originString: String {
        var components = URLComponents(url: origin, resolvingAgainstBaseURL: false) ?? URLComponents()
        components.path = ""
        components.query = nil
        components.fragment = nil
        if let host = components.host, let scheme = components.scheme {
            if let port = components.port {
                return "\(scheme)://\(host):\(port)"
            }
            return "\(scheme)://\(host)"
        }
        return origin.absoluteString
    }

    private static func origin(from url: URL) -> URL {
        var components = URLComponents(url: url, resolvingAgainstBaseURL: false) ?? URLComponents()
        components.fragment = nil
        components.query = nil
        components.path = ""
        return components.url ?? url
    }

    private static func accessToken(from url: URL) -> String? {
        guard let fragment = url.fragment, !fragment.isEmpty else { return nil }
        let pairs = fragment.split(separator: "&")
        for pair in pairs {
            let parts = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard parts.count == 2, parts[0] == "access_token" else { continue }
            let raw = String(parts[1])
            return raw.removingPercentEncoding ?? raw
        }
        return nil
    }
}
