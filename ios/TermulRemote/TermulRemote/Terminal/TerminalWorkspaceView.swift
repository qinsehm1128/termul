import SwiftUI

struct TerminalWorkspaceView: View {
    @Bindable var session: WorkspaceSession

    var body: some View {
        VStack(spacing: 0) {
            toolbar
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
                Button {
                    Task { await spawn() }
                } label: {
                    Text("New terminal")
                        .frame(minHeight: 44)
                }
                .buttonStyle(.borderedProminent)
                .tint(TermulTheme.accent)
                .padding(.bottom, 24)
            }
            shortcuts
        }
        .task(id: session.projects.active?.id) {
            guard let projectId = session.projects.active?.id else { return }
            await session.terminals.refresh(projectId: projectId)
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(2))
                await session.terminals.refresh(projectId: projectId)
            }
        }
    }

    private var toolbar: some View {
        HStack {
            if !session.terminals.terminals.isEmpty {
                Picker(String(localized: "Terminal"), selection: bindActive) {
                    ForEach(session.terminals.terminals) { item in
                        Text(item.title).tag(Optional(item.id))
                    }
                }
                .pickerStyle(.menu)
            }
            Spacer()
            if session.terminals.isConnecting {
                ProgressView()
            }
            Button {
                Task { await spawn() }
            } label: {
                Image(systemName: "plus")
                    .frame(minWidth: 44, minHeight: 44)
            }
            .accessibilityLabel(Text("New terminal"))
        }
        .padding(.horizontal, 12)
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

    private var bindActive: Binding<String?> {
        Binding(
            get: { session.terminals.activeId },
            set: { newId in
                session.terminals.activeId = newId
                if let newId {
                    Task { await session.terminals.open(newId) }
                }
            }
        )
    }

    private func shortcut(_ title: String, _ payload: String) -> some View {
        Button(title) {
            Task { await session.terminals.write(payload) }
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .disabled(session.terminals.activeId == nil)
    }

    private func spawn() async {
        guard let project = session.projects.active else {
            session.terminals.errorMessage = String(localized: "Pick a project first.")
            return
        }
        await session.terminals.spawn(projectId: project.id, cwd: project.path)
    }
}
