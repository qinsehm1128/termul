import SwiftUI

struct WorkspaceView: View {
    @Bindable var store: ConnectionStore
    let link: RemoteLink
    @State private var session: WorkspaceSession
    @State private var showProjects = false
    @State private var showFiles = false
    @Environment(\.scenePhase) private var scenePhase

    init(store: ConnectionStore, link: RemoteLink) {
        self.store = store
        self.link = link
        _session = State(initialValue: WorkspaceSession(origin: link.originURL))
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            content
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
        .sheet(isPresented: $showProjects) {
            ProjectPicker(session: session)
        }
        .sheet(isPresented: $showFiles) {
            FileBrowserView(session: session)
        }
        .alert(
            String(localized: "Could not load session"),
            isPresented: alertBinding
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(session.chat.errorMessage ?? session.projects.errorMessage ?? session.files.errorMessage ?? session.terminals.errorMessage ?? "")
        }
    }

    private var header: some View {
        VStack(spacing: 10) {
            HStack(spacing: 8) {
                Button {
                    store.disconnect()
                } label: {
                    Image(systemName: "chevron.backward")
                        .font(.body.bold())
                        .frame(minWidth: 44, minHeight: 44)
                }
                .accessibilityLabel(Text("Back to home"))

                VStack(alignment: .leading, spacing: 2) {
                    Text(link.title)
                        .font(.headline)
                        .lineLimit(1)
                    HStack(spacing: 6) {
                        Circle()
                            .fill(statusColor)
                            .frame(width: 8, height: 8)
                        Text(statusText)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 8)
                Button {
                    showProjects = true
                } label: {
                    Image(systemName: "square.stack")
                        .frame(minWidth: 44, minHeight: 44)
                }
                .accessibilityLabel(Text("Projects"))
                Button {
                    showFiles = true
                } label: {
                    Image(systemName: "folder")
                        .frame(minWidth: 44, minHeight: 44)
                }
                .accessibilityLabel(Text("Files"))
            }

            Picker(String(localized: "Workspace"), selection: $store.surface) {
                Text("Chat").tag(WorkspaceSurface.chat)
                Text("Terminal").tag(WorkspaceSurface.terminal)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(minHeight: 44)
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 10)
        .background(.ultraThinMaterial)
    }

    @ViewBuilder
    private var content: some View {
        switch session.phase {
        case .connecting, .idle:
            ProgressView(String(localized: "Connecting to host…"))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
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
        case .connected:
            switch store.surface {
            case .chat:
                ChatView(session: session)
            case .terminal:
                TerminalWorkspaceView(session: session)
            }
        }
    }

    private var statusColor: Color {
        switch session.phase {
        case .connected: .green
        case .connecting, .idle: .orange
        case .failed: .red
        }
    }

    private var statusText: String {
        switch session.phase {
        case .connected:
            session.projects.active?.name ?? link.originHost
        case .connecting, .idle:
            String(localized: "Connecting to host…")
        case .failed:
            String(localized: "Disconnected from the host.")
        }
    }

    private var alertBinding: Binding<Bool> {
        Binding(
            get: {
                session.chat.errorMessage != nil
                    || session.projects.errorMessage != nil
                    || session.files.errorMessage != nil
                    || session.terminals.errorMessage != nil
            },
            set: { presented in
                if !presented {
                    session.chat.errorMessage = nil
                    session.projects.errorMessage = nil
                    session.files.errorMessage = nil
                    session.terminals.errorMessage = nil
                }
            }
        )
    }
}
