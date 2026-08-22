import SwiftUI

struct TerminalWorkspaceView: View {
    @Bindable var session: WorkspaceSession
    @State private var focusToken: UInt64 = 0

    var body: some View {
        VStack(spacing: 0) {
            if let id = session.terminals.activeId {
                TerminalScreen(
                    terminalId: id,
                    buffered: session.terminals.pendingOutput[id] ?? Data(),
                    onSend: { text in
                        Task { await session.terminals.write(text) }
                    },
                    onResize: { cols, rows in
                        Task { await session.terminals.resize(cols: cols, rows: rows) }
                    },
                    onReady: { feed in
                        session.terminals.onFeed = { incomingId, data in
                            if incomingId == id {
                                feed(data)
                            }
                        }
                    },
                    focusToken: focusToken
                )
                .id(id)
                .onAppear {
                    session.terminals.pendingOutput[id] = nil
                }
            } else {
                ContentUnavailableView(
                    "No live terminal",
                    systemImage: "apple.terminal",
                    description: Text("Open a terminal on the desktop, or start a new one here.")
                )
                if session.workspace == .conversation || session.workspace == .project {
                    Button {
                        Task { await spawnTerminal() }
                    } label: {
                        Text("New terminal")
                            .frame(minHeight: 44)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(TermulTheme.accent)
                    .padding(.bottom, 24)
                }
            }
            TerminalInputDock(
                isEnabled: session.terminals.activeId != nil,
                onSend: { text in
                    Task { await session.terminals.write(text) }
                },
                onFocusTerminal: {
                    focusToken &+= 1
                }
            )
        }
    }

    private func spawnTerminal() async {
        switch session.workspace {
        case .conversation:
            guard let conversation = session.conversations.active else { return }
            await session.terminals.spawn(
                conversationId: conversation.id,
                projectId: conversation.projectId
            )
        case .project:
            guard let project = session.projects.active else { return }
            await session.terminals.spawn(
                conversationId: nil,
                projectId: project.id
            )
        case .home:
            return
        }
        if let id = session.terminals.activeId {
            await session.revealTerminal(id)
        }
    }
}
