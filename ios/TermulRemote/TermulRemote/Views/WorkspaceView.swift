import SwiftUI

struct WorkspaceView: View {
    @Bindable var store: ConnectionStore
    let link: RemoteLink
    @State private var session: WorkspaceSession
    @Environment(\.scenePhase) private var scenePhase

    init(store: ConnectionStore, link: RemoteLink) {
        self.store = store
        self.link = link
        _session = State(initialValue: WorkspaceSession(accessURL: link.accessURL))
    }

    var body: some View {
        Group {
            switch session.phase {
            case .connecting, .idle:
                ProgressView(String(localized: "Connecting to host…"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(TermulTheme.canvas.ignoresSafeArea())
            case .failed(let message):
                ContentUnavailableView {
                    Label("Could not load session", systemImage: "wifi.exclamationmark")
                } description: {
                    Text(message)
                } actions: {
                    Button("Retry") {
                        Task { await session.retry() }
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(TermulTheme.accent)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(TermulTheme.canvas.ignoresSafeArea())
            case .connected:
                switch session.workspace {
                case .home:
                    HostHomeView(session: session, store: store, link: link)
                case .conversation, .project:
                    SessionScreen(session: session)
                }
            }
        }
        .background(TermulTheme.canvas.ignoresSafeArea())
        .task(id: link.id) {
            await session.start()
        }
        .onChange(of: scenePhase) { _, phase in
            session.handleScene(isBackground: phase != .active)
        }
        .onDisappear {
            session.stop()
        }
        .alert(
            String(localized: "Could not load session"),
            isPresented: alertBinding
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(session.chat.errorMessage ?? session.conversations.errorMessage ?? session.projects.errorMessage ?? session.files.errorMessage ?? session.terminals.errorMessage ?? "")
        }
    }

    private var alertBinding: Binding<Bool> {
        Binding(
            get: {
                session.chat.errorMessage != nil
                    || session.conversations.errorMessage != nil
                    || session.projects.errorMessage != nil
                    || session.files.errorMessage != nil
                    || session.terminals.errorMessage != nil
            },
            set: { presented in
                if !presented {
                    session.chat.errorMessage = nil
                    session.conversations.errorMessage = nil
                    session.projects.errorMessage = nil
                    session.files.errorMessage = nil
                    session.terminals.errorMessage = nil
                }
            }
        )
    }
}
