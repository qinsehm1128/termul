import SwiftUI

struct TerminalWorkspaceView: View {
    @Bindable var session: WorkspaceSession

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
                    }
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
                if session.workspace == .conversation {
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
            shortcuts
        }
    }

    private var shortcuts: some View {
        HStack(spacing: 8) {
            shortcut("Esc", "\u{1b}")
            shortcut("Tab", "\t")
            shortcut("⌃C", "\u{3}")
            shortcut("⌃D", "\u{4}")
            shortcut("↑", "\u{1b}[A")
            shortcut("↓", "\u{1b}[B")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.ultraThinMaterial)
    }

    private func shortcut(_ title: String, _ payload: String) -> some View {
        Button(title) {
            Task { await session.terminals.write(payload) }
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .disabled(session.terminals.activeId == nil)
    }

    private func spawnTerminal() async {
        guard let conversation = session.conversations.active else { return }
        await session.terminals.spawn(
            conversationId: conversation.id,
            projectId: conversation.projectId
        )
        if let id = session.terminals.activeId {
            await session.revealTerminal(id)
        }
    }
}
