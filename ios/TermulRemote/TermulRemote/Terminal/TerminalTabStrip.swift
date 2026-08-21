import SwiftUI

struct TerminalTabStrip: View {
    @Bindable var session: WorkspaceSession

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(session.terminals.terminals) { item in
                    Button {
                        Task { await session.revealTerminal(item.id) }
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(item.title)
                                .font(.subheadline.weight(.semibold))
                                .lineLimit(1)
                            if let branch = item.gitBranch, !branch.isEmpty {
                                Text(branch)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .frame(minHeight: 44)
                        .background(
                            session.terminals.activeId == item.id && (session.workspace == .project || !session.showChat)
                                ? TermulTheme.accent.opacity(0.16)
                                : TermulTheme.surface
                        )
                        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                        .overlay(
                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                .stroke(TermulTheme.stroke, lineWidth: 1)
                        )
                    }
                    .buttonStyle(.plain)
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
            .padding(.vertical, 8)
        }
        .background(.ultraThinMaterial)
    }

    private func spawn() async {
        switch session.workspace {
        case .conversation:
            guard let conversation = session.conversations.active else { return }
            await session.terminals.spawn(
                conversationId: conversation.id,
                projectId: conversation.projectId
            )
        case .project:
            session.terminals.errorMessage = String(localized: "Open a new terminal on the desktop project. The phone watches it.")
            return
        case .home:
            return
        }
        if let id = session.terminals.activeId {
            await session.revealTerminal(id)
        }
    }
}
