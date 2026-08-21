import Observation
import SwiftUI
import WebKit

struct SessionView: View {
    let link: RemoteLink
    @Environment(\.openURL) private var openURL
    @State private var decider: SessionNavigationDecider
    @State private var page: WebPage

    init(link: RemoteLink) {
        self.link = link
        let decider = SessionNavigationDecider(allowedHost: link.accessURL.host())
        _decider = State(initialValue: decider)
        _page = State(initialValue: WebPage(configuration: .init(), navigationDecider: decider))
    }

    var body: some View {
        WebView(page)
            .navigationTitle(page.title ?? link.title)
            .navigationBarTitleDisplayMode(.inline)
            .overlay(alignment: .top) {
                if page.isLoading {
                    ProgressView(value: page.estimatedProgress)
                        .progressViewStyle(.linear)
                }
            }
            .task {
                do {
                    for try await _ in page.load(URLRequest(url: link.accessURL)) {}
                } catch {
                    // Keep the session visible so the operator can go back and rescan.
                }
            }
            .onChange(of: decider.urlToOpenExternally) { _, url in
                guard let url else { return }
                openURL(url)
                decider.urlToOpenExternally = nil
            }
    }
}

@Observable
@MainActor
final class SessionNavigationDecider: WebPage.NavigationDeciding {
    var urlToOpenExternally: URL?
    private let allowedHost: String?

    init(allowedHost: String?) {
        self.allowedHost = allowedHost
    }

    func decidePolicy(
        for action: WebPage.NavigationAction,
        preferences _: inout WebPage.NavigationPreferences
    ) async -> WKNavigationActionPolicy {
        guard let url = action.request.url else { return .cancel }
        if url.host() == allowedHost {
            return .allow
        }
        urlToOpenExternally = url
        return .cancel
    }
}
