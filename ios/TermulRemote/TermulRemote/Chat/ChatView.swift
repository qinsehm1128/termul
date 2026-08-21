import SwiftUI

struct ChatView: View {
    @Bindable var session: WorkspaceSession
    @State private var draft = ""
    @State private var showSessions = false

    var body: some View {
        VStack(spacing: 0) {
            sessionBar
            messageList
            permissionStack
            composer
        }
        .sheet(isPresented: $showSessions) {
            SessionListView(session: session)
        }
    }

    private var sessionBar: some View {
        HStack {
            Button {
                showSessions = true
            } label: {
                Label(currentTitle, systemImage: "bubble.left.and.bubble.right")
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
            }
            Spacer()
            Button {
                Task {
                    let cwd = session.projects.active?.path ?? session.chat.activeCwd
                    await session.chat.startNewChat(cwd: cwd, projectId: session.projects.active?.id)
                }
            } label: {
                Image(systemName: "square.and.pencil")
                    .frame(minWidth: 44, minHeight: 44)
            }
            .accessibilityLabel(Text("New chat"))
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 4)
    }

    private var currentTitle: String {
        session.chat.sessions.first(where: { $0.sessionId == session.chat.activeSessionId })?.displayTitle
            ?? String(localized: "Chat")
    }

    private var messageList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(session.chat.messages) { message in
                        MessageBubble(message: message)
                            .id(message.id)
                    }
                    ForEach(session.chat.tools) { tool in
                        ToolRow(card: tool)
                    }
                    if session.chat.isLoading {
                        ProgressView()
                            .frame(maxWidth: .infinity)
                    }
                }
                .padding(16)
            }
            .onChange(of: session.chat.messages.last?.id) { _, id in
                if let id {
                    withAnimation { proxy.scrollTo(id, anchor: .bottom) }
                }
            }
        }
    }

    @ViewBuilder
    private var permissionStack: some View {
        if !session.chat.permissions.isEmpty || !session.chat.questions.isEmpty {
            VStack(spacing: 10) {
                ForEach(session.chat.permissions) { card in
                    PermissionCardView(card: card) { option in
                        Task { await session.chat.respond(permission: card, optionId: option) }
                    }
                }
                ForEach(session.chat.questions) { card in
                    QuestionCardView(card: card) { value in
                        Task { await session.chat.answer(question: card, values: [value]) }
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
        }
    }

    private var composer: some View {
        HStack(alignment: .bottom, spacing: 10) {
            TextField("Message the host agent", text: $draft, axis: .vertical)
                .textInputAutocapitalization(.sentences)
                .lineLimit(1 ... 6)
                .padding(12)
                .background(TermulTheme.surface)
                .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            if session.chat.isSending {
                Button {
                    Task { await session.chat.cancel() }
                } label: {
                    Image(systemName: "stop.fill")
                        .frame(width: 44, height: 44)
                }
                .accessibilityLabel(Text("Cancel"))
            } else {
                Button {
                    let text = draft
                    draft = ""
                    Task { await session.chat.send(text) }
                } label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title)
                        .frame(width: 44, height: 44)
                }
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityLabel(Text("Send"))
            }
        }
        .padding(16)
        .background(.ultraThinMaterial)
    }
}

private struct MessageBubble: View {
    let message: ChatMessage

    var body: some View {
        HStack {
            if message.role == .user { Spacer(minLength: 48) }
            VStack(alignment: message.role == .user ? .trailing : .leading, spacing: 6) {
                Text(attributed)
                    .font(.body)
                    .textSelection(.enabled)
                if message.streaming {
                    ProgressView()
                        .controlSize(.mini)
                }
            }
            .padding(12)
            .background(background)
            .foregroundStyle(message.role == .thought ? .secondary : .primary)
            .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            if message.role != .user { Spacer(minLength: 48) }
        }
    }

    private var background: Color {
        switch message.role {
        case .user: TermulTheme.accent.opacity(0.16)
        case .thought: TermulTheme.surface
        case .agent: TermulTheme.surface
        }
    }

    private var attributed: AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: message.text, options: options))
            ?? AttributedString(message.text)
    }
}

private struct ToolRow: View {
    let card: ToolCard

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "wrench.and.screwdriver")
            Text(card.title)
                .lineLimit(2)
            Spacer()
            Text(card.status)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .font(.subheadline)
        .padding(10)
        .background(TermulTheme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

private struct PermissionCardView: View {
    let card: PermissionCard
    var onChoose: (String?) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Permission required")
                .font(.headline)
            Text(card.title)
                .font(.subheadline)
                .foregroundStyle(.secondary)
            HStack {
                ForEach(card.options) { option in
                    Button(option.name) { onChoose(option.id) }
                        .buttonStyle(.borderedProminent)
                        .tint(TermulTheme.accent)
                }
                Button("Deny") { onChoose(nil) }
                    .buttonStyle(.bordered)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(TermulTheme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
    }
}

private struct QuestionCardView: View {
    let card: QuestionCard
    var onChoose: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(card.question)
                .font(.headline)
            ForEach(card.options) { option in
                Button(option.label) { onChoose(option.id) }
                    .buttonStyle(.bordered)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(TermulTheme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
    }
}

private struct SessionListView: View {
    @Bindable var session: WorkspaceSession
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List(session.chat.sessions) { item in
                Button {
                    Task {
                        await session.chat.open(item)
                        dismiss()
                    }
                } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(item.displayTitle)
                            .foregroundStyle(.primary)
                        Text(item.cwd ?? item.sessionId)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
            }
            .navigationTitle(String(localized: "Chats"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .overlay {
                if session.chat.sessions.isEmpty {
                    ContentUnavailableView(
                        "No chats yet",
                        systemImage: "bubble.left",
                        description: Text("Start a chat, or open one that is already running on the desk.")
                    )
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}
