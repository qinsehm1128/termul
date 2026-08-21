import SwiftUI

struct FileBrowserView: View {
    @Bindable var session: WorkspaceSession
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if let preview = session.files.preview {
                    ScrollView {
                        Text(preview.content)
                            .font(.body.monospaced())
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(16)
                            .textSelection(.enabled)
                    }
                    .navigationTitle(session.files.previewName ?? String(localized: "File"))
                } else {
                    List(session.files.entries) { entry in
                        Button {
                            Task { await session.files.open(entry) }
                        } label: {
                            Label(entry.name, systemImage: entry.isDirectory ? "folder" : "doc")
                                .foregroundStyle(.primary)
                        }
                    }
                    .overlay {
                        if session.files.isLoading {
                            ProgressView()
                        } else if session.files.entries.isEmpty {
                            ContentUnavailableView(
                                "Empty folder",
                                systemImage: "folder",
                                description: Text("This directory has no visible files.")
                            )
                        }
                    }
                    .navigationTitle(session.files.crumbs.last?.name ?? String(localized: "Files"))
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                if session.files.preview != nil || session.files.crumbs.count > 1 {
                    ToolbarItem(placement: .navigationBarLeading) {
                        Button("Back") {
                            if session.files.preview != nil {
                                session.files.preview = nil
                                session.files.previewName = nil
                            } else if session.files.crumbs.count > 1 {
                                let parent = session.files.crumbs[session.files.crumbs.count - 2]
                                Task { await session.files.popTo(parent) }
                            }
                        }
                    }
                }
            }
        }
        .task {
            if let path = session.projects.active?.path, session.files.crumbs.isEmpty {
                await session.files.openRoot(path)
            }
        }
    }
}
