import SwiftUI

struct SessionScreen: View {
    @Bindable var session: WorkspaceSession
    @Environment(\.horizontalSizeClass) private var sizeClass
    @State private var showFiles = false

    var body: some View {
        VStack(spacing: 0) {
            header
            TerminalTabStrip(session: session)
            content
        }
        .background(TermulTheme.canvas.ignoresSafeArea())
        .sheet(isPresented: $showFiles) {
            FileBrowserView(session: session)
        }
        .task(id: workspaceTaskId) {
            await session.refreshActiveTerminals()
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(2))
                await session.refreshActiveTerminals()
            }
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Button {
                session.leaveWorkspace()
            } label: {
                Image(systemName: "chevron.backward")
                    .font(.body.bold())
                    .frame(minWidth: 44, minHeight: 44)
            }
            .accessibilityLabel(Text("Back"))

            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.headline)
                    .lineLimit(1)
                if let subtitle {
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            if session.workspace == .conversation {
                Button {
                    session.revealChat()
                } label: {
                    Image(systemName: session.showChat ? "bubble.left.and.bubble.right.fill" : "bubble.left.and.bubble.right")
                        .frame(minWidth: 44, minHeight: 44)
                }
                .accessibilityLabel(Text("Chat"))
            }
            Button {
                showFiles = true
            } label: {
                Image(systemName: "folder")
                    .frame(minWidth: 44, minHeight: 44)
            }
            .accessibilityLabel(Text("Files"))
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 6)
        .background(.ultraThinMaterial)
    }

    @ViewBuilder
    private var content: some View {
        if session.workspace == .project {
            TerminalWorkspaceView(session: session)
        } else if sizeClass == .regular && session.showChat {
            HStack(spacing: 0) {
                TerminalWorkspaceView(session: session)
                Divider()
                ChatView(session: session, embedded: true)
                    .frame(minWidth: 320, idealWidth: 380, maxWidth: 420)
            }
        } else if session.showChat {
            ChatView(session: session, embedded: true)
        } else {
            TerminalWorkspaceView(session: session)
        }
    }

    private var title: String {
        switch session.workspace {
        case .conversation:
            session.conversations.active?.displayTitle ?? String(localized: "Session")
        case .project:
            session.projects.active?.name ?? String(localized: "Project")
        case .home:
            String(localized: "Workspace")
        }
    }

    private var subtitle: String? {
        switch session.workspace {
        case .conversation:
            session.conversations.active?.workspaceCwd
        case .project:
            session.projects.active?.path
        case .home:
            nil
        }
    }

    private var workspaceTaskId: String {
        switch session.workspace {
        case .conversation:
            "conversation:\(session.conversations.active?.id ?? "")"
        case .project:
            "project:\(session.projects.active?.id ?? "")"
        case .home:
            "home"
        }
    }
}
